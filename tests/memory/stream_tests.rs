use std::sync::Arc;
use std::time::Duration;

use xworkflow::core::variable_pool::{Segment, SegmentStream, StreamEvent};

use super::helpers::{assert_arc_dropped, with_timeout, DropCounter};

#[test]
fn test_helper_drop_counter_smoke() {
    let (counter, count) = DropCounter::new();
    drop(counter);
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);

    let arc = Arc::new(());
    let weak = Arc::downgrade(&arc);
    drop(arc);
    assert_arc_dropped(&weak, "arc");
}

#[tokio::test]
async fn test_stream_writer_drop_without_end() {
    let (stream, writer) = SegmentStream::channel();
    writer.send(Segment::String("chunk1".into())).await;
    drop(writer);

    let result = with_timeout("stream collect", Duration::from_secs(2), stream.collect()).await;
    assert!(result.is_err(), "expected stream to fail after writer drop");
}

#[tokio::test]
async fn test_stream_reader_drop_decrements_count() {
    let (stream, writer) = SegmentStream::channel();
    let r1 = stream.reader();
    let r2 = stream.reader();
    let r3 = stream.reader();

    assert_eq!(stream.debug_readers_count(), 3);

    drop(r1);
    assert_eq!(stream.debug_readers_count(), 2);
    drop(r2);
    assert_eq!(stream.debug_readers_count(), 1);
    drop(r3);
    assert_eq!(stream.debug_readers_count(), 0);

    writer.end(Segment::String("done".into())).await;
    let result = stream.collect().await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_stream_arc_cleanup_after_completion() {
    let (stream, writer) = SegmentStream::channel();
    let weak = stream.debug_state_weak();

    writer.end(Segment::String("final".into())).await;
    let _ = stream.collect().await;

    drop(writer);
    drop(stream);

    assert!(weak.is_dropped(), "stream state Arc should be released");
}

#[tokio::test]
async fn test_stream_multiple_readers_partial_drop() {
    let (stream, writer) = SegmentStream::channel();
    let mut r1 = stream.reader();
    let r2 = stream.reader();

    writer.send(Segment::String("chunk".into())).await;
    writer.end(Segment::String("done".into())).await;

    let mut events = Vec::new();
    while let Some(e) = r1.next().await {
        events.push(e);
        if matches!(events.last(), Some(StreamEvent::End(_))) {
            break;
        }
    }

    assert_eq!(events.len(), 2);

    drop(r1);
    drop(r2);
    assert_eq!(stream.debug_readers_count(), 0);
}
