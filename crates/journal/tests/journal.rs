use journal::synthetic::{SyntheticWriter, cut};
use journal::{
    Batch, Frame, Reader, Step, Torn, Writer, encode_frame, is_final, reclaimable, segment_name,
    segments,
};
use wire::{Item, Snapshot};

fn step(key: &str, text: &str) -> Step {
    Step {
        items: vec![Item {
            key: key.into(),
            text: text.into(),
            kind: wire::Kind::ClaudeSdk as i32,
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn keys(batch: &Batch) -> Vec<String> {
    batch
        .frames
        .iter()
        .map(|(_, step)| step.items[0].key.clone())
        .collect()
}

fn frame_len(step: &Step) -> u64 {
    encode_frame(step).len() as u64
}

#[test]
fn segments_are_named_by_the_global_offset_of_their_first_byte() {
    let dir = tempfile::tempdir().unwrap();
    let one = step("a", &"x".repeat(40));
    let len = frame_len(&one);
    // Two frames fit a segment; the third rotates.
    let mut writer = Writer::open(dir.path(), 2 * len).unwrap();
    let mut ends = Vec::new();
    for key in ["a", "b", "c", "d", "e"] {
        ends.push(writer.append(&step(key, &"x".repeat(40))).unwrap());
    }
    assert_eq!(ends, (1..=5).map(|n| n * len).collect::<Vec<_>>());
    assert_eq!(segments(dir.path()).unwrap(), vec![0, 2 * len, 4 * len]);
    assert_eq!(segment_name(0), "0000000000");
    assert!(dir.path().join(segment_name(2 * len)).exists());

    assert!(is_final(dir.path(), 0).unwrap());
    assert!(is_final(dir.path(), 2 * len).unwrap());
    assert!(!is_final(dir.path(), 4 * len).unwrap());

    let batch = Reader::new(dir.path(), 0).read_to_end().unwrap();
    assert_eq!(keys(&batch), ["a", "b", "c", "d", "e"]);
    assert_eq!(
        batch.frames.iter().map(|(end, _)| *end).collect::<Vec<_>>(),
        ends
    );
    assert_eq!(batch.torn, None);
}

#[test]
fn a_frame_larger_than_a_segment_gets_a_segment_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = Writer::open(dir.path(), 16).unwrap();
    let big = step("big", &"y".repeat(100));
    writer.append(&step("s", "")).unwrap();
    let after_small = writer.offset();
    writer.append(&big).unwrap();
    writer.append(&step("t", "")).unwrap();
    assert_eq!(
        segments(dir.path()).unwrap(),
        vec![0, after_small, after_small + frame_len(&big)]
    );
    let batch = Reader::new(dir.path(), 0).read_to_end().unwrap();
    assert_eq!(keys(&batch), ["s", "big", "t"]);
}

#[test]
fn a_reader_starts_at_any_frame_boundary_in_any_segment() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = Writer::open(dir.path(), 64).unwrap();
    let mut ends = vec![0];
    for n in 0..20 {
        ends.push(writer.append(&step(&format!("k{n}"), "text")).unwrap());
    }
    assert!(segments(dir.path()).unwrap().len() > 3);
    for (index, &cursor) in ends.iter().enumerate() {
        let mut reader = Reader::new(dir.path(), cursor);
        let batch = reader.read_to_end().unwrap();
        let expected = (index..20).map(|n| format!("k{n}")).collect::<Vec<_>>();
        assert_eq!(keys(&batch), expected, "from {cursor}");
        assert_eq!(reader.cursor(), writer.offset());
    }
}

#[test]
fn a_reader_follows_a_live_writer_across_rotations() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = Writer::open(dir.path(), 64).unwrap();
    let mut reader = Reader::new(dir.path(), 0);
    let mut seen = Vec::new();
    for n in 0..30 {
        writer.append(&step(&format!("k{n}"), "text")).unwrap();
        if n % 7 == 0 {
            seen.extend(keys(&reader.read_to_end().unwrap()));
        }
    }
    seen.extend(keys(&reader.read_to_end().unwrap()));
    assert_eq!(seen, (0..30).map(|n| format!("k{n}")).collect::<Vec<_>>());
    let end = writer.offset();
    assert_eq!(
        reader.read_to_end().unwrap(),
        Batch {
            seen: end,
            ..Batch::default()
        }
    );
}

#[test]
fn a_torn_tail_with_the_writer_alive_is_read_once_the_frame_completes() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = SyntheticWriter::open(dir.path(), 1 << 20).unwrap();
    writer.append(&step("a", "whole")).unwrap();
    let last_whole = writer.offset();
    let big = step("b", &"streaming text ".repeat(20));
    let full = writer.write_partial(&big, 7).unwrap();

    let mut reader = Reader::new(dir.path(), 0);
    let batch = reader.read_to_end().unwrap();
    assert_eq!(keys(&batch), ["a"]);
    assert_eq!(
        batch.torn,
        Some(Torn {
            last_whole,
            skipped: false
        })
    );
    assert_eq!(reader.cursor(), last_whole);

    // Still torn on a second look; never returned as a frame.
    let again = reader.read_to_end().unwrap();
    assert!(again.frames.is_empty());
    assert_eq!(reader.cursor(), last_whole);

    let end = writer.finish_partial().unwrap();
    assert_eq!(end, last_whole + full as u64);
    let batch = reader.read_to_end().unwrap();
    assert_eq!(keys(&batch), ["b"]);
    assert_eq!(batch.frames[0].1, big);
    assert_eq!(batch.torn, None);
    assert_eq!(reader.cursor(), end);
}

#[test]
fn a_torn_tail_with_the_writer_dead_is_cut_off_when_a_writer_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = Writer::open(dir.path(), 1 << 20).unwrap();
    writer.append(&step("a", "one")).unwrap();
    let last_whole = writer.append(&step("b", "two")).unwrap();
    writer.append(&step("c", "three, lost mid-write")).unwrap();
    drop(writer);
    // The process died partway through writing c.
    cut(dir.path(), last_whole + 5).unwrap();

    let mut reader = Reader::new(dir.path(), 0);
    let batch = reader.read_to_end().unwrap();
    assert_eq!(keys(&batch), ["a", "b"]);
    assert_eq!(
        batch.torn,
        Some(Torn {
            last_whole,
            skipped: false
        })
    );

    // A restarted agent process reopens the journal: the torn bytes go and
    // its re-derived records follow the last whole frame.
    let mut writer = Writer::open(dir.path(), 1 << 20).unwrap();
    assert_eq!(writer.offset(), last_whole);
    writer.append(&step("c", "three, re-derived")).unwrap();
    let batch = reader.read_to_end().unwrap();
    assert_eq!(keys(&batch), ["c"]);
    assert_eq!(batch.frames[0].1.items[0].text, "three, re-derived");
    assert_eq!(batch.torn, None);
}

#[test]
fn a_reader_never_returns_a_partial_frame_wherever_the_journal_is_cut() {
    let source = tempfile::tempdir().unwrap();
    let mut writer = Writer::open(source.path(), 80).unwrap();
    let mut ends = Vec::new();
    for n in 0..12 {
        ends.push(
            writer
                .append(&step(&format!("k{n}"), &"z".repeat(n * 3)))
                .unwrap(),
        );
    }
    let total = writer.offset();
    for byte in 0..=total {
        let dir = tempfile::tempdir().unwrap();
        for start in segments(source.path()).unwrap() {
            std::fs::copy(
                source.path().join(segment_name(start)),
                dir.path().join(segment_name(start)),
            )
            .unwrap();
        }
        cut(dir.path(), byte).unwrap();
        let mut reader = Reader::new(dir.path(), 0);
        let batch = reader.read_to_end().unwrap();
        let whole = ends.iter().filter(|&&end| end <= byte).count();
        assert_eq!(
            keys(&batch),
            (0..whole).map(|n| format!("k{n}")).collect::<Vec<_>>(),
            "cut at {byte}"
        );
        let last_whole = if whole == 0 { 0 } else { ends[whole - 1] };
        assert_eq!(reader.cursor(), last_whole, "cut at {byte}");
        assert_eq!(
            batch.torn.is_some(),
            byte != last_whole,
            "cut at {byte}: torn reported only for bytes past a whole frame"
        );
    }
}

#[test]
fn a_synthetic_cut_reopens_the_writer_at_the_last_whole_frame() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = SyntheticWriter::open(dir.path(), 48).unwrap();
    let mut ends = Vec::new();
    for n in 0..8 {
        ends.push(writer.append(&step(&format!("k{n}"), "abc")).unwrap());
    }
    writer.cut_at(ends[4] + 3).unwrap();
    assert_eq!(writer.offset(), ends[4]);
    writer.append(&step("k5-again", "abc")).unwrap();
    let batch = Reader::new(dir.path(), 0).read_to_end().unwrap();
    assert_eq!(keys(&batch), ["k0", "k1", "k2", "k3", "k4", "k5-again"]);
    assert_eq!(batch.torn, None);
}

#[test]
fn torn_bytes_in_a_final_segment_are_skipped_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = Writer::open(dir.path(), 1 << 20).unwrap();
    writer.append(&step("a", "one")).unwrap();
    let last_whole = writer.offset();
    drop(writer);
    // Garbage the next frame never finished, then a later segment: the
    // earlier one can never complete.
    std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join(segment_name(0)))
        .and_then(|mut file| std::io::Write::write_all(&mut file, &[0x05, 0x0a]))
        .unwrap();
    let next = last_whole + 2;
    let mut later = Writer::open(dir.path().join("later"), 1 << 20).unwrap();
    later.append(&step("b", "two")).unwrap();
    std::fs::rename(
        dir.path().join("later").join(segment_name(0)),
        dir.path().join(segment_name(next)),
    )
    .unwrap();

    let mut reader = Reader::new(dir.path(), 0);
    let batch = reader.read_to_end().unwrap();
    assert_eq!(keys(&batch), ["a", "b"]);
    assert_eq!(
        batch.torn,
        Some(Torn {
            last_whole,
            skipped: true
        })
    );
}

#[test]
fn an_empty_step_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = Writer::open(dir.path(), 1 << 20).unwrap();
    assert_eq!(writer.append(&Step::default()).unwrap(), 0);
    assert!(segments(dir.path()).unwrap().is_empty());
    let with_snapshot = Step {
        snapshot: Some(Snapshot {
            kind: wire::Kind::Codex as i32,
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(writer.append(&with_snapshot).unwrap() > 0);
}

#[test]
fn segments_below_a_durable_cursor_are_reclaimable_except_the_kept_ones() {
    let dir = tempfile::tempdir().unwrap();
    let one = step("a", "0123456789");
    let len = frame_len(&one);
    let mut writer = Writer::open(dir.path(), len).unwrap();
    for _ in 0..5 {
        writer.append(&one).unwrap();
    }
    assert_eq!(
        segments(dir.path()).unwrap(),
        (0..5).map(|n| n * len).collect::<Vec<_>>()
    );
    // A cursor inside the third segment: the first two lie wholly below it.
    assert_eq!(
        reclaimable(dir.path(), 2 * len + 1, 0).unwrap(),
        vec![0, len]
    );
    assert_eq!(reclaimable(dir.path(), 2 * len + 1, 1).unwrap(), vec![0]);
    // The newest segment is never below anything: it may still grow.
    assert_eq!(
        reclaimable(dir.path(), 5 * len, 0).unwrap(),
        vec![0, len, 2 * len, 3 * len]
    );
    // A reader whose cursor predates the oldest remaining segment resumes
    // at it.
    for start in reclaimable(dir.path(), 5 * len, 2).unwrap() {
        std::fs::remove_file(dir.path().join(segment_name(start))).unwrap();
    }
    let batch = Reader::new(dir.path(), 0).read_to_end().unwrap();
    assert_eq!(batch.frames.len(), 3);
}

#[test]
fn a_limited_read_takes_a_backlog_in_pieces_across_rotations() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = Writer::open(dir.path(), 64).unwrap();
    for n in 0..30 {
        writer.append(&step(&format!("k{n}"), "text")).unwrap();
    }
    let mut reader = Reader::new(dir.path(), 0);
    let mut seen = Vec::new();
    let mut reads = 0;
    loop {
        let batch = reader.read_up_to(7).unwrap();
        reads += 1;
        assert!(batch.frames.len() <= 7);
        assert_eq!(batch.more, batch.frames.len() == 7 && seen.len() + 7 < 30);
        seen.extend(keys(&batch));
        if !batch.more {
            break;
        }
    }
    assert_eq!(seen, (0..30).map(|n| format!("k{n}")).collect::<Vec<_>>());
    assert_eq!(reads, 5);
    assert_eq!(reader.cursor(), writer.offset());
    assert_eq!(
        reader.read_up_to(7).unwrap(),
        Batch {
            seen: writer.offset(),
            ..Batch::default()
        }
    );
}

#[test]
fn a_frame_of_a_given_length_reads_back_as_its_step() {
    let short = step("k", "short");
    let natural = frame_len(&short) as usize;
    for len in natural..natural + 300 {
        let frame = journal::encode_frame_exact(&short, len).expect("the step fits");
        assert_eq!(frame.len(), len);
        assert_eq!(
            journal::decode_frame(&frame),
            Frame::Whole(Box::new(short.clone()), len)
        );
    }
    assert_eq!(journal::encode_frame_exact(&short, natural - 1), None);
}

#[test]
fn a_read_says_how_far_it_saw_the_journal_torn_tail_included() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = SyntheticWriter::open(dir.path(), 1 << 20).unwrap();
    writer.append(&step("a", "one")).unwrap();
    let whole = frame_len(&step("a", "one"));
    writer.write_partial(&step("b", "two"), 3).unwrap();
    let batch = Reader::new(dir.path(), 0).read_to_end().unwrap();
    assert_eq!(keys(&batch), ["a"]);
    assert_eq!(batch.seen, whole + 3);
}
