from collections.abc import Iterable as _Iterable
from collections.abc import Mapping as _Mapping
from typing import (
    ClassVar as _ClassVar,
)

from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from google.protobuf.internal import containers as _containers

DESCRIPTOR: _descriptor.FileDescriptor

class Profile(_message.Message):
    __slots__ = (
        "sample_type",
        "sample",
        "mapping",
        "location",
        "function",
        "string_table",
        "drop_frames",
        "keep_frames",
        "time_nanos",
        "duration_nanos",
        "period_type",
        "period",
        "comment",
        "default_sample_type",
        "doc_url",
    )
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
    def __init__(
        self,
        sample_type: _Iterable[ValueType | _Mapping] | None = ...,
        sample: _Iterable[Sample | _Mapping] | None = ...,
        mapping: _Iterable[Mapping | _Mapping] | None = ...,
        location: _Iterable[Location | _Mapping] | None = ...,
        function: _Iterable[Function | _Mapping] | None = ...,
        string_table: _Iterable[str] | None = ...,
        drop_frames: int | None = ...,
        keep_frames: int | None = ...,
        time_nanos: int | None = ...,
        duration_nanos: int | None = ...,
        period_type: ValueType | _Mapping | None = ...,
        period: int | None = ...,
        comment: _Iterable[int] | None = ...,
        default_sample_type: int | None = ...,
        doc_url: int | None = ...,
    ) -> None: ...

class ValueType(_message.Message):
    __slots__ = ("type", "unit")
    TYPE_FIELD_NUMBER: _ClassVar[int]
    UNIT_FIELD_NUMBER: _ClassVar[int]
    type: int
    unit: int
    def __init__(self, type: int | None = ..., unit: int | None = ...) -> None: ...

class Sample(_message.Message):
    __slots__ = ("location_id", "value", "label")
    LOCATION_ID_FIELD_NUMBER: _ClassVar[int]
    VALUE_FIELD_NUMBER: _ClassVar[int]
    LABEL_FIELD_NUMBER: _ClassVar[int]
    location_id: _containers.RepeatedScalarFieldContainer[int]
    value: _containers.RepeatedScalarFieldContainer[int]
    label: _containers.RepeatedCompositeFieldContainer[Label]
    def __init__(
        self,
        location_id: _Iterable[int] | None = ...,
        value: _Iterable[int] | None = ...,
        label: _Iterable[Label | _Mapping] | None = ...,
    ) -> None: ...

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
    def __init__(
        self,
        key: int | None = ...,
        str: int | None = ...,
        num: int | None = ...,
        num_unit: int | None = ...,
    ) -> None: ...

class Mapping(_message.Message):
    __slots__ = (
        "id",
        "memory_start",
        "memory_limit",
        "file_offset",
        "filename",
        "build_id",
        "has_functions",
        "has_filenames",
        "has_line_numbers",
        "has_inline_frames",
    )
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
    def __init__(
        self,
        id: int | None = ...,
        memory_start: int | None = ...,
        memory_limit: int | None = ...,
        file_offset: int | None = ...,
        filename: int | None = ...,
        build_id: int | None = ...,
        has_functions: bool = ...,
        has_filenames: bool = ...,
        has_line_numbers: bool = ...,
        has_inline_frames: bool = ...,
    ) -> None: ...

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
    def __init__(
        self,
        id: int | None = ...,
        mapping_id: int | None = ...,
        address: int | None = ...,
        line: _Iterable[Line | _Mapping] | None = ...,
        is_folded: bool = ...,
    ) -> None: ...

class Line(_message.Message):
    __slots__ = ("function_id", "line", "column")
    FUNCTION_ID_FIELD_NUMBER: _ClassVar[int]
    LINE_FIELD_NUMBER: _ClassVar[int]
    COLUMN_FIELD_NUMBER: _ClassVar[int]
    function_id: int
    line: int
    column: int
    def __init__(
        self,
        function_id: int | None = ...,
        line: int | None = ...,
        column: int | None = ...,
    ) -> None: ...

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
    def __init__(
        self,
        id: int | None = ...,
        name: int | None = ...,
        system_name: int | None = ...,
        filename: int | None = ...,
        start_line: int | None = ...,
    ) -> None: ...
