//! Execution status types.

mod segment;
mod status;

pub use segment::{
    FileCategory, FileSegment, FileTransferMethod, Segment, SegmentArray, SegmentObject,
    SegmentStream, StreamEvent, StreamLimits, StreamReader, StreamStateWeak, StreamStatus,
    StreamWriter,
};
pub use status::ExecutionStatus;
