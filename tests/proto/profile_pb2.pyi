from google.protobuf.internal import containers as _containers
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from typing import ClassVar as _ClassVar, Iterable as _Iterable, Mapping as _Mapping, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class Profile(_message.Message):
    __slots__ = ("sample_type", "sample", "mapping", "location", "function", "string_table", "drop_frames", "keep_frames", "time_nanos", "duration_nanos", "period_type", "period", "comment", "default_sample_type", "doc_url")
    SAMPLE_TYPE_FIELD_NUMBER: _ClassVar[int]
    SAMPLE_FIELD_NUMBER: _ClassVar[int]
    MAPPING_FIELD_NUMBER: _ClassVar[int]
    LOCATION_FIELD_NUMBER: _ClassVar[int]
    FUNCTION_FIELD_NUMBER: _ClassVar[int]
    STRING_TABLE_FIELD_NUMBER: _ClassVar[int]
    DROP_FRAMES_FIELD_NUMBER: _ClassVar[int]
    KEEP_FRAMES_FIELD_NUMBER: _ClassVar[int]
    TIME_NANOS_FIELD_NUMBER: _ClassVar[int]
    DURATION_NANOS_FIELD_NUMBER: _ClassVar[int]
    PERIOD_TYPE_FIELD_NUMBER: _ClassVar[int]
    PERIOD_FIELD_NUMBER: _ClassVar[int]
    COMMENT_FIELD_NUMBER: _ClassVar[int]
    DEFAULT_SAMPLE_TYPE_FIELD_NUMBER: _ClassVar[int]
    DOC_URL_FIELD_NUMBER: _ClassVar[int]
    sample_type: _containers.RepeatedCompositeFieldContainer[ValueType]
    sample: _containers.RepeatedCompositeFieldContainer[Sample]
    mapping: _containers.RepeatedCompositeFieldContainer[Mapping]
    location: _containers.RepeatedCompositeFieldContainer[Location]
    function: _containers.RepeatedCompositeFieldContainer[Function]
    string_table: _containers.RepeatedScalarFieldContainer[str]
    drop_frames: int
    keep_frames: int
    time_nanos: int
    duration_nanos: int
    period_type: ValueType
    period: int
    comment: _containers.RepeatedScalarFieldContainer[int]
    default_sample_type: int
    doc_url: int
    def __init__(self, sample_type: _Optional[_Iterable[_Union[ValueType, _Mapping]]] = ..., sample: _Optional[_Iterable[_Union[Sample, _Mapping]]] = ..., mapping: _Optional[_Iterable[_Union[Mapping, _Mapping]]] = ..., location: _Optional[_Iterable[_Union[Location, _Mapping]]] = ..., function: _Optional[_Iterable[_Union[Function, _Mapping]]] = ..., string_table: _Optional[_Iterable[str]] = ..., drop_frames: _Optional[int] = ..., keep_frames: _Optional[int] = ..., time_nanos: _Optional[int] = ..., duration_nanos: _Optional[int] = ..., period_type: _Optional[_Union[ValueType, _Mapping]] = ..., period: _Optional[int] = ..., comment: _Optional[_Iterable[int]] = ..., default_sample_type: _Optional[int] = ..., doc_url: _Optional[int] = ...) -> None: ...

class ValueType(_message.Message):
    __slots__ = ("type", "unit")
    TYPE_FIELD_NUMBER: _ClassVar[int]
    UNIT_FIELD_NUMBER: _ClassVar[int]
    type: int
    unit: int
    def __init__(self, type: _Optional[int] = ..., unit: _Optional[int] = ...) -> None: ...

class Sample(_message.Message):
    __slots__ = ("location_id", "value", "label")
    LOCATION_ID_FIELD_NUMBER: _ClassVar[int]
    VALUE_FIELD_NUMBER: _ClassVar[int]
    LABEL_FIELD_NUMBER: _ClassVar[int]
    location_id: _containers.RepeatedScalarFieldContainer[int]
    value: _containers.RepeatedScalarFieldContainer[int]
    label: _containers.RepeatedCompositeFieldContainer[Label]
    def __init__(self, location_id: _Optional[_Iterable[int]] = ..., value: _Optional[_Iterable[int]] = ..., label: _Optional[_Iterable[_Union[Label, _Mapping]]] = ...) -> None: ...

class Label(_message.Message):
    __slots__ = ("key", "str", "num", "num_unit")
    KEY_FIELD_NUMBER: _ClassVar[int]
    STR_FIELD_NUMBER: _ClassVar[int]
    NUM_FIELD_NUMBER: _ClassVar[int]
    NUM_UNIT_FIELD_NUMBER: _ClassVar[int]
    key: int
    str: int
    num: int
    num_unit: int
    def __init__(self, key: _Optional[int] = ..., str: _Optional[int] = ..., num: _Optional[int] = ..., num_unit: _Optional[int] = ...) -> None: ...

class Mapping(_message.Message):
    __slots__ = ("id", "memory_start", "memory_limit", "file_offset", "filename", "build_id", "has_functions", "has_filenames", "has_line_numbers", "has_inline_frames")
    ID_FIELD_NUMBER: _ClassVar[int]
    MEMORY_START_FIELD_NUMBER: _ClassVar[int]
    MEMORY_LIMIT_FIELD_NUMBER: _ClassVar[int]
    FILE_OFFSET_FIELD_NUMBER: _ClassVar[int]
    FILENAME_FIELD_NUMBER: _ClassVar[int]
    BUILD_ID_FIELD_NUMBER: _ClassVar[int]
    HAS_FUNCTIONS_FIELD_NUMBER: _ClassVar[int]
    HAS_FILENAMES_FIELD_NUMBER: _ClassVar[int]
    HAS_LINE_NUMBERS_FIELD_NUMBER: _ClassVar[int]
    HAS_INLINE_FRAMES_FIELD_NUMBER: _ClassVar[int]
    id: int
    memory_start: int
    memory_limit: int
    file_offset: int
    filename: int
    build_id: int
    has_functions: bool
    has_filenames: bool
    has_line_numbers: bool
    has_inline_frames: bool
    def __init__(self, id: _Optional[int] = ..., memory_start: _Optional[int] = ..., memory_limit: _Optional[int] = ..., file_offset: _Optional[int] = ..., filename: _Optional[int] = ..., build_id: _Optional[int] = ..., has_functions: bool = ..., has_filenames: bool = ..., has_line_numbers: bool = ..., has_inline_frames: bool = ...) -> None: ...

class Location(_message.Message):
    __slots__ = ("id", "mapping_id", "address", "line", "is_folded")
    ID_FIELD_NUMBER: _ClassVar[int]
    MAPPING_ID_FIELD_NUMBER: _ClassVar[int]
    ADDRESS_FIELD_NUMBER: _ClassVar[int]
    LINE_FIELD_NUMBER: _ClassVar[int]
    IS_FOLDED_FIELD_NUMBER: _ClassVar[int]
    id: int
    mapping_id: int
    address: int
    line: _containers.RepeatedCompositeFieldContainer[Line]
    is_folded: bool
    def __init__(self, id: _Optional[int] = ..., mapping_id: _Optional[int] = ..., address: _Optional[int] = ..., line: _Optional[_Iterable[_Union[Line, _Mapping]]] = ..., is_folded: bool = ...) -> None: ...

class Line(_message.Message):
    __slots__ = ("function_id", "line", "column")
    FUNCTION_ID_FIELD_NUMBER: _ClassVar[int]
    LINE_FIELD_NUMBER: _ClassVar[int]
    COLUMN_FIELD_NUMBER: _ClassVar[int]
    function_id: int
    line: int
    column: int
    def __init__(self, function_id: _Optional[int] = ..., line: _Optional[int] = ..., column: _Optional[int] = ...) -> None: ...

class Function(_message.Message):
    __slots__ = ("id", "name", "system_name", "filename", "start_line")
    ID_FIELD_NUMBER: _ClassVar[int]
    NAME_FIELD_NUMBER: _ClassVar[int]
    SYSTEM_NAME_FIELD_NUMBER: _ClassVar[int]
    FILENAME_FIELD_NUMBER: _ClassVar[int]
    START_LINE_FIELD_NUMBER: _ClassVar[int]
    id: int
    name: int
    system_name: int
    filename: int
    start_line: int
    def __init__(self, id: _Optional[int] = ..., name: _Optional[int] = ..., system_name: _Optional[int] = ..., filename: _Optional[int] = ..., start_line: _Optional[int] = ...) -> None: ...
