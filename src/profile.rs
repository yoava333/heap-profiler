use crate::proto::*;
use crate::stack::StackFrame;
use crate::sync::Mutex;
use crate::ProfileEntry;
use hashbrown::HashMap;
use once_cell::sync::Lazy;
use prost::Message;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList};
use std::io::prelude::*;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

// Constants for sampling
pub const DEFAULT_SAMPLE_RATE: i32 = 128 * 1024; // 128 KiB - reduced sampling frequency for better performance

/// Inverse inclusion probability, computed per allocation rather than from
/// an average size. The same weight follows the allocation through its free.
pub(crate) fn sample_weight(size: usize, rate: i32) -> (f64, f64) {
    if rate <= 1 {
        return (1.0, size as f64);
    }
    let probability = -(-(size.max(1) as f64) / rate as f64).exp_m1();
    (1.0 / probability, size as f64 / probability)
}

pub(crate) static PROFILE_TRANSACTION: Mutex<()> = Mutex::new(());
pub(crate) static GENERATION: AtomicU64 = AtomicU64::new(0);

pub static PROFILE_DATA: Lazy<Mutex<HashMap<u64, MemProfileRecord>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
pub static SAMPLE_RATE: AtomicI32 = AtomicI32::new(DEFAULT_SAMPLE_RATE);
pub static TOTAL_ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct MemProfileRecord {
    pub alloc_bytes: i64,
    pub free_bytes: i64,
    pub alloc_objects: i64,
    pub free_objects: i64,
    pub scaled_alloc_objects: f64,
    pub scaled_alloc_bytes: f64,
    pub scaled_free_objects: f64,
    pub scaled_free_bytes: f64,
    pub stack: Box<[StackFrame]>,
}

impl MemProfileRecord {
    pub fn new(stack: &[StackFrame]) -> Self {
        Self {
            alloc_bytes: 0,
            free_bytes: 0,
            alloc_objects: 0,
            free_objects: 0,
            scaled_alloc_objects: 0.0,
            scaled_alloc_bytes: 0.0,
            scaled_free_objects: 0.0,
            scaled_free_bytes: 0.0,
            stack: stack.to_vec().into_boxed_slice(),
        }
    }
}

/// get_profile_data()
/// --
///
/// This function returns the heap profile data as a list of tuples.
/// Values are RAW sampled counts (not scaled by sampling probability);
/// get_pprof_data() returns scaled values.
#[pyfunction]
pub fn get_profile_data(py: Python<'_>) -> PyResult<Py<PyList>> {
    let _guard = crate::recursion_guard::MallocRecursionGuard::new();
    // Snapshot under a short lock; building Python objects while holding the
    // global lock would stall every sampled allocation in the process (and
    // any sampled allocation made *by* this thread would self-deadlock).
    let records: Vec<MemProfileRecord> = {
        let profile_data = PROFILE_DATA.lock();
        profile_data.values().cloned().collect()
    };

    let mut result = Vec::with_capacity(records.len());
    for record in &records {
        let stack_frames: Vec<(String, String, i32)> = record
            .stack
            .iter()
            .map(|frame| {
                (
                    frame.filename.to_string(),
                    frame.function_name.to_string(),
                    frame.lineno,
                )
            })
            .collect();

        result.push(ProfileEntry {
            alloc_bytes: record.alloc_bytes,
            free_bytes: record.free_bytes,
            alloc_objects: record.alloc_objects,
            free_objects: record.free_objects,
            stack_frames,
        });
    }

    // Convert while suppression is active; returning Vec would allocate the
    // Python list/classes only AFTER the guard had been dropped.
    Ok(PyList::new(py, result)?.unbind())
}

/// reset_profile_data()
/// --
///
/// Clears all accumulated profile data, per-pointer metadata, and the total
/// allocation counter. After a reset, inuse values mean "allocated since the
/// reset": live allocations sampled before it are forgotten and their later
/// frees are not counted.
#[pyfunction]
pub fn reset_profile_data() {
    let _guard = crate::recursion_guard::MallocRecursionGuard::new();
    let _transaction = PROFILE_TRANSACTION.lock();
    GENERATION.fetch_add(1, Ordering::Relaxed);
    PROFILE_DATA.lock().clear();
    crate::metadata_store::clear();
    crate::interner::STRING_INTERNER.lock().clear();
    TOTAL_ALLOC_BYTES.store(0, Ordering::Relaxed);
}

/// pprof_heap()
/// --
///
/// This function returns the heap profile data as a pprof-compatible binary format.
#[pyfunction]
#[pyo3(signature = (tags=None, compress=false))]
pub fn get_pprof_data(
    py: Python<'_>,
    tags: Option<std::collections::HashMap<String, String>>,
    compress: bool,
) -> PyResult<Py<PyBytes>> {
    let _guard = crate::recursion_guard::MallocRecursionGuard::new();
    // Snapshot under a short lock (see get_profile_data); record clones are
    // cheap because frame strings are shared Arc references.
    let records: Vec<MemProfileRecord> = {
        let profile_data = PROFILE_DATA.lock();
        profile_data.values().cloned().collect()
    };

    //
    // 1. Prepare string interner
    //
    // We'll store all strings in a single table and keep track of their IDs in a HashMap.
    let mut string_index_map = HashMap::new();

    // Helper to get or insert a string into the table
    let mut string_table: Vec<String> = Vec::new();
    // string_table[0] = "" is typical for pprof; so let's add it explicitly
    string_table.push("".to_string());
    string_index_map.insert("".to_string(), 0);

    let mut next_string_id = 1;

    fn get_string_id(
        s: &str,
        string_index_map: &mut HashMap<String, i64>,
        string_table: &mut Vec<String>,
        next_string_id: &mut i64,
    ) -> i64 {
        if let Some(&id) = string_index_map.get(s) {
            id
        } else {
            let id = *next_string_id;
            *next_string_id += 1;
            string_index_map.insert(s.to_string(), id);
            // Make sure the string_table can hold the new index
            if string_table.len() <= id as usize {
                string_table.resize(id as usize + 1, String::new());
            }
            string_table[id as usize] = s.to_string();
            id
        }
    }

    // Pre-register the standard strings we know we'll need:
    let alloc_objects_id = get_string_id(
        "alloc_objects",
        &mut string_index_map,
        &mut string_table,
        &mut next_string_id,
    );
    let count_id = get_string_id(
        "count",
        &mut string_index_map,
        &mut string_table,
        &mut next_string_id,
    );
    let alloc_space_id = get_string_id(
        "alloc_space",
        &mut string_index_map,
        &mut string_table,
        &mut next_string_id,
    );
    let bytes_id = get_string_id(
        "bytes",
        &mut string_index_map,
        &mut string_table,
        &mut next_string_id,
    );
    let inuse_objects_id = get_string_id(
        "inuse_objects",
        &mut string_index_map,
        &mut string_table,
        &mut next_string_id,
    );
    let inuse_space_id = get_string_id(
        "inuse_space",
        &mut string_index_map,
        &mut string_table,
        &mut next_string_id,
    );
    let space_id = get_string_id(
        "space",
        &mut string_index_map,
        &mut string_table,
        &mut next_string_id,
    );

    //
    // 2. Initialize the Profile
    //
    let mut profile = Profile {
        sample_type: vec![
            ValueType {
                r#type: alloc_objects_id,
                unit: count_id,
            },
            ValueType {
                r#type: alloc_space_id,
                unit: bytes_id,
            },
            ValueType {
                r#type: inuse_objects_id,
                unit: count_id,
            },
            ValueType {
                r#type: inuse_space_id,
                unit: bytes_id,
            },
        ],
        sample: Vec::new(),
        mapping: Vec::new(),
        location: Vec::new(),
        function: Vec::new(),
        // We'll fill in the correct string_table at the end:
        string_table: Vec::new(),
        drop_frames: 0,
        keep_frames: 0,
        time_nanos: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64,
        duration_nanos: 0,
        period_type: Some(ValueType {
            r#type: space_id,
            unit: bytes_id,
        }),
        period: SAMPLE_RATE.load(Ordering::Relaxed) as i64,
        comment: Vec::new(),
        default_sample_type: inuse_space_id,
    };

    //
    // 3. Add tags to the profile
    //
    if let Some(tags) = tags {
        for (k, v) in tags {
            let line = format!("{}: {}", k, v);
            profile.comment.push(get_string_id(
                &line,
                &mut string_index_map,
                &mut string_table,
                &mut next_string_id,
            ));
        }
    }
    profile.comment.push(get_string_id(
        &format!(
            "total_alloc_bytes: {}",
            TOTAL_ALLOC_BYTES.load(Ordering::Relaxed)
        ),
        &mut string_index_map,
        &mut string_table,
        &mut next_string_id,
    ));

    //
    // 4. Dedup structures:
    //
    // We'll keep a map for (filename) -> mapping_id
    let mut mapping_map: HashMap<String, u64> = HashMap::new();

    // We'll keep a map for (filename_id, function_name_id, start_line) -> function_id
    let mut function_map: HashMap<(i64, i64, i64), u64> = HashMap::new();

    // We'll keep a map for (function_id, line) -> location_id
    // (You might also want to incorporate "is_folded" or other attributes if they vary.)
    let mut location_map: HashMap<(u64, i64), u64> = HashMap::new();

    // We'll track the next IDs to assign when we create new mapping/function/location
    let mut next_mapping_id = 1u64;
    let mut next_function_id = 1u64;
    let mut next_location_id = 1u64;

    //
    // 5. For each record in the data, build up the sample
    //
    for record in &records {
        let mut location_ids = Vec::new();

        // For each frame in the stack
        for frame in record.stack.iter() {
            // Clean up the frame data
            // – You might do your <frozen> logic here, but omitted for brevity
            let filename = &frame.filename;
            let lineno = frame.lineno as i64;
            let func_name = if frame.function_name.is_empty() {
                "<unknown>"
            } else {
                &frame.function_name
            };

            // Get string IDs
            let filename_id = get_string_id(
                filename,
                &mut string_index_map,
                &mut string_table,
                &mut next_string_id,
            );
            let func_name_id = get_string_id(
                func_name,
                &mut string_index_map,
                &mut string_table,
                &mut next_string_id,
            );

            //
            // 4a. Deduplicate the mapping
            //
            // Suppose you want one mapping per filename (typical for pprof from a single script).
            let filename_str = filename.to_string();
            let mapping_id = if let Some(&mid) = mapping_map.get(&filename_str) {
                mid
            } else {
                let mid = next_mapping_id;
                next_mapping_id += 1;

                // Insert into the profile's mapping vector
                profile.mapping.push(Mapping {
                    id: mid,
                    memory_start: 0,
                    memory_limit: 0,
                    file_offset: 0,
                    filename: filename_id,
                    build_id: 0,
                    has_functions: true,
                    has_filenames: true,
                    has_line_numbers: true,
                    has_inline_frames: false,
                });

                // Remember in the map
                mapping_map.insert(filename_str, mid);
                mid
            };

            //
            // 4b. Deduplicate the function
            //
            let func_key = (filename_id, func_name_id, frame.first_lineno as i64);
            let function_id = if let Some(&fid) = function_map.get(&func_key) {
                fid
            } else {
                let fid = next_function_id;
                next_function_id += 1;

                profile.function.push(Function {
                    id: fid,
                    name: func_name_id,        // The function name
                    system_name: func_name_id, // Could differ if you like
                    filename: filename_id,
                    start_line: frame.first_lineno as i64,
                });

                function_map.insert(func_key, fid);
                fid
            };

            //
            // 4c. Deduplicate the location
            //
            let loc_key = (function_id, lineno);
            let location_id = if let Some(&lid) = location_map.get(&loc_key) {
                lid
            } else {
                let lid = next_location_id;
                next_location_id += 1;

                profile.location.push(Location {
                    id: lid,
                    mapping_id,
                    address: 0,
                    line: vec![Line {
                        function_id,
                        line: lineno,
                    }],
                    is_folded: false,
                });

                location_map.insert(loc_key, lid);
                lid
            };

            location_ids.push(location_id);
        }

        let scaled_alloc_objects = record.scaled_alloc_objects.round() as i64;
        let scaled_alloc_bytes = record.scaled_alloc_bytes.round() as i64;
        let inuse_objects = (record.scaled_alloc_objects - record.scaled_free_objects)
            .max(0.0)
            .round() as i64;
        let inuse_bytes = (record.scaled_alloc_bytes - record.scaled_free_bytes)
            .max(0.0)
            .round() as i64;

        // Finally, push the Sample
        profile.sample.push(Sample {
            location_id: location_ids,
            value: vec![
                scaled_alloc_objects,
                scaled_alloc_bytes,
                inuse_objects,
                inuse_bytes,
            ],
            label: Vec::new(),
        });
    }

    //
    // 6. Now that we've built the profile, attach the final string table
    //
    profile.string_table = string_table;

    //
    // 7. Encode the profile
    //
    let buf = profile.encode_to_vec();
    if compress {
        let mut compressed = Vec::new();
        let mut encoder =
            flate2::write::GzEncoder::new(&mut compressed, flate2::Compression::default());

        encoder.write_all(&buf).map_err(|e| {
            PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
                "Failed to encode profile: {}",
                e
            ))
        })?;
        encoder.finish().map_err(|e| {
            PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
                "Failed to finish encoding profile: {}",
                e
            ))
        })?;
        Ok(PyBytes::new(py, &compressed).unbind())
    } else {
        Ok(PyBytes::new(py, &buf).unbind())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_are_stable_for_extreme_rates_and_sizes() {
        assert_eq!(sample_weight(100, 1), (1.0, 100.0));
        assert_eq!(sample_weight(1 << 20, 1024), (1.0, (1 << 20) as f64));
        let (count, bytes) = sample_weight(1, i32::MAX);
        assert!(count.is_finite() && bytes.is_finite());
        assert!(count > 2_000_000_000.0);
        assert!(sample_weight(0, 1024).0.is_finite());
    }
}
