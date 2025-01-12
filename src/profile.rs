use crate::proto::*;
use crate::stack::StackFrame;
use lazy_static::lazy_static;
use prost::Message;
use pyo3::prelude::*;
use std::collections::HashMap;
use std::sync::Mutex;

// Constants for sampling
pub const DEFAULT_SAMPLE_RATE: i32 = 512 * 1024; // 512KB

lazy_static! {
    pub static ref PROFILE_DATA: Mutex<HashMap<u64, MemProfileRecord>> =
        Mutex::new(HashMap::with_capacity(179_999));
}

#[derive(Debug)]
pub struct MemProfileRecord {
    pub alloc_bytes: i64,
    pub free_bytes: i64,
    pub alloc_objects: i64,
    pub free_objects: i64,
    pub stack0: [StackFrame; 128],
}

impl MemProfileRecord {
    pub fn new(stack: [StackFrame; 128]) -> Self {
        Self {
            alloc_bytes: 0,
            free_bytes: 0,
            alloc_objects: 0,
            free_objects: 0,
            stack0: stack,
        }
    }
}

#[pyfunction]
pub fn get_profile_data() -> PyResult<Vec<(i64, i64, i64, i64, Vec<(String, i32)>)>> {
    let profile_data = PROFILE_DATA.lock().map_err(|_| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>("Failed to lock profile data")
    })?;

    let mut result = Vec::new();
    for record in profile_data.values() {
        let stack_frames: Vec<(String, i32)> = record
            .stack0
            .iter()
            .take_while(|frame| !frame.filename.is_empty())
            .map(|frame| (frame.filename.clone(), frame.lineno))
            .collect();

        result.push((
            record.alloc_bytes,
            record.free_bytes,
            record.alloc_objects,
            record.free_objects,
            stack_frames,
        ));
    }

    Ok(result)
}

#[pyfunction]
pub fn write_heap_proto() -> PyResult<Vec<u8>> {
    let profile_data = PROFILE_DATA.lock().map_err(|_| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>("Failed to lock profile data")
    })?;

    let mut string_index_map = HashMap::new();
    string_index_map.insert("".to_string(), 0i64);
    let mut next_string_id = 1i64;

    // Pre-register all strings we'll need
    let alloc_objects_id = {
        let s = "alloc_objects".to_string();
        let idx = next_string_id;
        next_string_id += 1;
        string_index_map.insert(s, idx);
        idx
    };

    let count_id = {
        let s = "count".to_string();
        let idx = next_string_id;
        next_string_id += 1;
        string_index_map.insert(s, idx);
        idx
    };

    let alloc_space_id = {
        let s = "alloc_space".to_string();
        let idx = next_string_id;
        next_string_id += 1;
        string_index_map.insert(s, idx);
        idx
    };

    let bytes_id = {
        let s = "bytes".to_string();
        let idx = next_string_id;
        next_string_id += 1;
        string_index_map.insert(s, idx);
        idx
    };

    let inuse_objects_id = {
        let s = "inuse_objects".to_string();
        let idx = next_string_id;
        next_string_id += 1;
        string_index_map.insert(s, idx);
        idx
    };

    let inuse_space_id = {
        let s = "inuse_space".to_string();
        let idx = next_string_id;
        next_string_id += 1;
        string_index_map.insert(s, idx);
        idx
    };

    let space_id = {
        let s = "space".to_string();
        let idx = next_string_id;
        next_string_id += 1;
        string_index_map.insert(s, idx);
        idx
    };

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
        string_table: vec![], // Will be filled at the end
        drop_frames: 0,
        keep_frames: 0,
        time_nanos: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64,
        duration_nanos: 0,
        period_type: Some(ValueType {
            r#type: space_id,
            unit: bytes_id,
        }),
        period: DEFAULT_SAMPLE_RATE as i64,
        comment: Vec::new(),
        default_sample_type: 3, // inuse_space
    };

    let mut next_location_id = 1u64;
    let mut next_function_id = 1u64;

    // Process each profile record
    for record in profile_data.values() {
        let mut location_ids = Vec::new();

        // Process stack frames
        for frame in record.stack0.iter().take_while(|f| !f.filename.is_empty()) {
            // Create function
            let function_id = next_function_id;
            next_function_id += 1;

            // Create a more descriptive function name that includes the line number
            let name = format!("{}:{}", frame.filename, frame.lineno);
            let name_id = if let Some(&idx) = string_index_map.get(&name) {
                idx
            } else {
                let idx = next_string_id;
                next_string_id += 1;
                string_index_map.insert(name, idx);
                idx
            };

            let filename_id = if let Some(&idx) = string_index_map.get(&frame.filename) {
                idx
            } else {
                let idx = next_string_id;
                next_string_id += 1;
                string_index_map.insert(frame.filename.clone(), idx);
                idx
            };

            profile.function.push(Function {
                id: function_id,
                name: name_id,
                system_name: name_id,
                filename: filename_id,
                start_line: frame.lineno as i64,
            });

            // Add mapping information
            profile.mapping.push(Mapping {
                id: function_id,
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

            // Create location with mapping reference
            let location_id = next_location_id;
            next_location_id += 1;

            profile.location.push(Location {
                id: location_id,
                mapping_id: function_id,
                address: 0,
                line: vec![Line {
                    function_id,
                    line: frame.lineno as i64,
                }],
                is_folded: false,
            });

            location_ids.push(location_id);
        }

        // Calculate inuse values
        let inuse_objects = record.alloc_objects - record.free_objects;
        let inuse_bytes = record.alloc_bytes - record.free_bytes;

        // Add sample
        profile.sample.push(Sample {
            location_id: location_ids,
            value: vec![
                record.alloc_objects,
                record.alloc_bytes,
                inuse_objects,
                inuse_bytes,
            ],
            label: Vec::new(),
        });
    }

    // Build string table at the end
    let mut string_table = vec!["".to_string()];
    string_table.resize(string_index_map.len(), String::new());
    for (s, &idx) in &string_index_map {
        string_table[idx as usize] = s.clone();
    }
    profile.string_table = string_table;

    // Encode the profile to bytes
    let mut buf = Vec::new();
    profile.encode(&mut buf).map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
            "Failed to encode profile: {}",
            e
        ))
    })?;

    Ok(buf)
}
