use crate::profile::SAMPLE_RATE;
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
#[derive(Clone, Copy)]
struct SamplerState {
    remaining: i64,
    rate: i32,
}
thread_local! {
    // No destructor: allocation callbacks can run from C++ TLS destructors.
    static RANDOM_STATE: Cell<u64> = const { Cell::new(0) };
    static SAMPLER: Cell<SamplerState> = const { Cell::new(SamplerState { remaining: 0, rate: 0 }) };
}

// SplitMix64 is used only for statistical sampling, not for cryptography.
// State lives in a const-initialized, non-dropping TLS Cell. rand::thread_rng
// can panic after its destructor has run while C++ TLS destructors still malloc.
static SEED_SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn mix_random(mut value: u64) -> u64 {
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}

fn fast_exp_rand(mean: i32) -> i64 {
    if mean <= 0 {
        return 0;
    }
    let random = RANDOM_STATE.with(|cell| {
        let mut state = cell.get();
        if state == 0 {
            let sequence = SEED_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let thread = unsafe { libc::pthread_self() } as usize as u64;
            let process = unsafe { libc::getpid() } as u64;
            state = mix_random(sequence ^ thread ^ process.rotate_left(32));
        }
        state = state.wrapping_add(0x9e3779b97f4a7c15);
        cell.set(state);
        mix_random(state)
    });
    // 53 random bits avoid the 26-bit generator's inflated minimum-interval
    // mass at large rates (32x too frequent for one-byte requests at i32::MAX).
    let uniform = (random >> 11) as f64 * (1.0 / 9007199254740992.0);
    exp_interval(mean, uniform)
}

fn exp_interval(mean: i32, uniform: f64) -> i64 {
    (-(-uniform).ln_1p() * mean as f64) as i64 + 1
}

pub(crate) fn reset_sampler_after_fork() {
    RANDOM_STATE.with(|state| state.set(0));
    SAMPLER.with(|state| {
        state.set(SamplerState {
            remaining: 0,
            rate: 0,
        })
    });
}

#[inline]
pub(super) fn should_sample(size: usize, rate: i32) -> bool {
    if rate == 1 {
        return true;
    }
    // Zero-byte requests still count as objects. Match sample_weight's minimum
    // inclusion probability without changing the recorded requested byte size.
    let size = size.max(1);
    // Keep the budget and configuration in one TLS slot: on macOS each TLS
    // access otherwise crosses a thread-local variable resolver.
    SAMPLER.with(|sampler| {
        let mut state = sampler.get();
        if state.rate != rate {
            state = SamplerState {
                remaining: fast_exp_rand(rate),
                rate,
            };
        }
        let sampled = state.remaining <= 0 || size >= state.remaining as usize;
        if sampled {
            state.remaining = fast_exp_rand(rate);
        } else {
            state.remaining -= size as i64;
        }
        sampler.set(state);
        sampled
    })
}

pub(super) fn configure(sample_rate: i32) {
    SAMPLE_RATE.store(sample_rate, Ordering::Relaxed);
    SAMPLER.with(|sampler| {
        sampler.set(SamplerState {
            remaining: fast_exp_rand(sample_rate),
            rate: sample_rate,
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::DEFAULT_SAMPLE_RATE;
    #[test]
    fn fast_exp_rand_is_positive() {
        for _ in 0..10_000 {
            assert!(fast_exp_rand(DEFAULT_SAMPLE_RATE) > 0);
        }
        assert_eq!(fast_exp_rand(0), 0);
        assert_eq!(fast_exp_rand(-5), 0);
    }

    #[test]
    fn tiny_sampling_probabilities_retain_precision() {
        let rate = i32::MAX;
        let probability = -(-1.0 / rate as f64).exp_m1();
        assert_eq!(exp_interval(rate, probability * 0.5), 1);
        assert_eq!(exp_interval(rate, probability * 1.5), 2);
        assert_eq!(exp_interval(rate, probability * 2.5), 3);
    }

    #[test]
    fn huge_allocations_are_always_sampled() {
        let _serial = crate::GLOBAL_TEST_LOCK.lock();
        SAMPLE_RATE.store(DEFAULT_SAMPLE_RATE, Ordering::Relaxed);
        SAMPLER.with(|sampler| {
            sampler.set(SamplerState {
                remaining: fast_exp_rand(DEFAULT_SAMPLE_RATE),
                rate: DEFAULT_SAMPLE_RATE,
            })
        });
        // 3 GiB allocation: larger than any sane counter value, must sample
        assert!(should_sample(3 * 1024 * 1024 * 1024, DEFAULT_SAMPLE_RATE));
    }
}
