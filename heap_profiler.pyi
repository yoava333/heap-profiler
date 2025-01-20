from typing import List, Tuple

class ProfileEntry:
    alloc_bytes: int
    alloc_objects: int
    free_bytes: int
    free_objects: int
    stack_frames: List[Tuple[str, str, int]]  # (filename, function, lineno)

def start_tracing() -> None: ...
def get_profile_data() -> List[ProfileEntry]: ...
def get_pprof_data(compress: bool = False) -> bytes: ...
