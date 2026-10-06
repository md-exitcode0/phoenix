//! Provider-backed multi-round tool loop (Hermes + OpenCode patterns).

mod envelope;

pub(crate) use envelope::{
    native_tool_error_content, native_tool_result_content, push_native_errors_for_calls,
};
