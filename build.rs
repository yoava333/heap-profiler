fn main() {
    // Adds the expected cfg to differenciate between CPython versions
    // now I can use #[cfg(Py_3_10)] or similar to check the version
    pyo3_build_config::print_expected_cfgs();
    pyo3_build_config::use_pyo3_cfgs();
}
