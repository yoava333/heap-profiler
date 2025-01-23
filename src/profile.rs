use crate::proto::*;
use crate::stack::StackFrame;
use crate::ProfileEntry;
use hashbrown::HashMap;
use lazy_static::lazy_static;
use prost::Message;
use pyo3::prelude::*;
use std::io::prelude::*;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;

// Constants for sampling
pub const DEFAULT_SAMPLE_RATE: i32 = 512 * 1024; // 512Kib - reduced sampling frequency for better performance

lazy_static! {
    pub static ref PROFILE_DATA: Mutex<HashMap<u64, MemProfileRecord>> =
        Mutex::new(HashMap::with_capacity(179_999));
    pub static ref SAMPLE_RATE: AtomicI32 = AtomicI32::new(DEFAULT_SAMPLE_RATE);
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

/// get_profile_data()
/// --
///
/// This function returns the heap profile data as a list of tuples.
#[pyfunction]
pub fn get_profile_data() -> PyResult<Vec<ProfileEntry>> {
    let profile_data = PROFILE_DATA.lock().map_err(|_| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>("Failed to lock profile data")
    })?;

    let mut result = Vec::new();
    for record in profile_data.values() {
        let stack_frames: Vec<(String, String, i32)> = record
            .stack0
            .iter()
            .take_while(|frame| !frame.filename.is_empty())
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

    Ok(result)
}

/// pprof_heap()
/// --
///
/// This function returns the heap profile data as a pprof-compatible binary format.
#[pyfunction]
#[pyo3(signature = (tags=None, compress=false))]
pub fn get_pprof_data(tags: Option<HashMap<String, String>>, compress: bool) -> PyResult<Vec<u8>> {
    let profile_data = PROFILE_DATA.lock().map_err(|_| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>("Failed to lock profile data")
    })?;

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
            .unwrap()
            .as_nanos() as i64,
        duration_nanos: 0,
        period_type: Some(ValueType {
            r#type: space_id,
            unit: bytes_id,
        }),
        period: SAMPLE_RATE.load(Ordering::Relaxed) as i64,
        comment: Vec::new(),
        default_sample_type: 3, // inuse_space
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
    for record in profile_data.values() {
        let mut location_ids = Vec::new();

        // For each frame in the stack
        for frame in record.stack0.iter().take_while(|f| !f.filename.is_empty()) {
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
            let func_key = (filename_id, func_name_id, lineno);
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
                    start_line: lineno,
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
                    mapping_id: mapping_id,
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

        // Calculate inuse values
        let inuse_objects = record.alloc_objects - record.free_objects;
        let inuse_bytes = record.alloc_bytes - record.free_bytes;

        // Finally, push the Sample
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
        Ok(compressed)
    } else {
        Ok(buf)
    }
}
