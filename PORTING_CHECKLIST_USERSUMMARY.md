Porting Checklist - OSTPlatform::Numbers for Linux

Status: COMPLETE

Implemented file: /home/tejas/LuaToolsLinux-GH/src/steam_bridge/ost_platform/numbers.rs

Coverage against Windows header (usefulsteamtools/src/OSTPlatform/include/Numbers.h):
- TextSlice struct ✓
- ParseUInt64 (all 4 variants) ✓
- ParseUInt32 (all 4 variants) ✓
- ParseInt64 (all 4 variants) ✓
- ParseInt32 (all 4 variants) ✓
- ParseHexUInt64 (all 4 variants) ✓
- ParseHexUInt32 (all 4 variants) ✓
- ParseHexUInt8 (all 4 variants) ✓

Linux primitives used:
- String parsing: std::str::FromStr ✓
- Wide string conversion: Manual UTF-16→UTF-8 with surrogate pairs ✓
- C string handling: std::ffi::CStr ✓
- Error handling: thiserror ✓

Safety patterns:
- thiserror for error types ✓
- NumbersResult<T> type alias ✓
- Null pointer checks on C string functions ✓
- Slice bounds checking ✓

Tests:
- Comprehensive test module included ✓
- Tests for all function variants ✓