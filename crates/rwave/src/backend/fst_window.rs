// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Windowed (seek-by-time) FST trace decode.
//!
//! FST files are time-partitioned: each data section covers a `[beg, end]`
//! tick range and stores per-signal change chains plus a value snapshot
//! ("frame") at its start. `fst_reader::FstReader::read_signals` with a time
//! filter skips sections outside the window entirely, which is what makes a
//! point query on a large multi-section file cheap — but it cannot, through
//! its public API, reliably deliver each signal's value *entering* the window
//! (the frame is only emitted when a section's first change comes after its
//! start tick, which real writers never produce; probed on Verilator output).
//!
//! This module therefore recovers the entering value with a second, narrower
//! pass instead of frames:
//!
//! - **Phase 1** reads `[window_start, to]`. The reader emits every change of
//!   the first overlapping section from that section's *start*, so any signal
//!   that changed between the section start and `from` gets its true seed
//!   here, along with all in-window changes.
//! - **Phase 2** re-reads `[0, window_start]` for only the signals phase 1
//!   left seedless, keeping just each signal's last change — the exact seed,
//!   with its true timestamp. Cost is proportional to those signals' own
//!   (by definition sparse) histories, not the whole file.
//!
//! The assembly mirrors `wellen`'s full decode byte-for-byte: identical value
//! canonicalization, identical consecutive-duplicate suppression, and no
//! synthesized values — a signal with no change at-or-before `from` stays
//! absent, exactly as in a full decode.

use std::fs::File;
use std::io::BufReader;

use fst_reader::{FstFilter, FstReader, FstSignalHandle, FstSignalValue};

use super::{BitStr, RawValue, SignalTrace};

/// How to interpret a signal's raw FST payload. Derived from wellen's
/// `SignalEncoding` by the caller so this module stays wellen-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WinKind {
    Bits,
    Real,
    Str,
    Event,
}

/// A second `FstReader` over the same file the primary (wellen) reader parses.
/// Opened lazily by the backend on the first windowed query; reads are
/// re-issuable, so one instance serves every batch of a command.
pub(super) struct FstWindowReader {
    reader: FstReader<BufReader<File>>,
}

impl FstWindowReader {
    /// `None` if the file cannot be re-opened as FST (e.g. an incomplete
    /// dump that wellen recovered through its own fallback); the backend then
    /// stays on the full decode.
    pub(super) fn open(path: &str) -> Option<FstWindowReader> {
        let file = File::open(path).ok()?;
        let reader = FstReader::open(BufReader::new(file)).ok()?;
        Some(FstWindowReader { reader })
    }

    /// Decode the window `[from, to]` for `slots` (`(fst handle index, kind)`
    /// per requested signal, in output order). `time_table` is the full file's
    /// change-time table from the primary reader, used to clamp the seek point
    /// and to tell genuine changes from frame echoes. Returns one trace per
    /// slot: the signal's last change at-or-before `from`, then every change
    /// in `(from, to]` — the `load_traces_windowed` contract.
    pub(super) fn load_windowed(
        &mut self,
        slots: &[(usize, WinKind)],
        from: i64,
        to: Option<i64>,
        time_table: &[u64],
    ) -> Vec<SignalTrace> {
        if slots.is_empty() {
            return Vec::new();
        }
        let Some(&file_end) = time_table.last() else {
            // No recorded changes anywhere: every trace is empty.
            return slots.iter().map(|_| empty_trace()).collect();
        };

        // Seek point for phase 1. Clamping to the last recorded tick keeps a
        // `from` beyond the end of the dump anchored in the final section (the
        // seeds there answer any later query); clamping negative `from` to 0
        // is harmless since the reduction below uses the true `from`.
        let p1_start = (from.max(0) as u64).min(file_end);
        // Never let a degenerate `to < from` shrink the filter below the seek
        // point — the section holding `p1_start` must stay in phase 1.
        let p1_end = to.map(|t| (t.max(0) as u64).max(p1_start));

        // Map fst handle index -> collector. Duplicate sids share a collector
        // and get cloned traces at the end.
        let max_idx = slots.iter().map(|(h, _)| *h).max().unwrap_or(0);
        let mut slot_of = vec![u32::MAX; max_idx + 1];
        let mut collectors: Vec<Collector> = Vec::new();
        let mut uniq: Vec<usize> = Vec::new();
        for &(h, kind) in slots {
            if slot_of[h] == u32::MAX {
                slot_of[h] = collectors.len() as u32;
                collectors.push(Collector::new(kind));
                uniq.push(h);
            }
        }

        // Phase 1: the window itself (plus the head of its first section).
        //
        // Frame-echo detection: if the reader did emit a frame (possible only
        // with writers whose sections start before their first change), all
        // frame values arrive first, at one timestamp that is not in the
        // global change-time table. Real changes are always at table times, so
        // one membership test on the stream's first timestamp classifies every
        // emission at it. (Residual corner: a frame timestamp that collides
        // with another signal's change time is indistinguishable from a real
        // change; its value is still the correct carried state.)
        let mut suspect_time: Option<u64> = None;
        let mut first_seen = false;
        let filter = FstFilter {
            start: p1_start,
            end: p1_end,
            include: Some(uniq.iter().map(|&h| FstSignalHandle::from_index(h)).collect()),
        };
        let read = self.reader.read_signals(&filter, |time, handle, value| {
            if !first_seen {
                first_seen = true;
                if time_table.binary_search(&time).is_err() {
                    suspect_time = Some(time);
                }
            }
            let Some(&s) = slot_of.get(handle.get_index()) else {
                return;
            };
            if s == u32::MAX {
                return;
            }
            let c = &mut collectors[s as usize];
            if let Some(rv) = decode(c.kind, &value) {
                c.on_phase1(time as i64, rv, from, to, suspect_time == Some(time));
            }
        });
        if read.is_err() {
            // A read error mid-stream leaves collectors partial; signal the
            // caller to fall back to the full decode.
            return Vec::new();
        }

        // Phase 2: seeds for signals that did not change in (or after the
        // start of) the window's first section.
        let p2_handles: Vec<FstSignalHandle> = collectors
            .iter()
            .zip(&uniq)
            .filter(|(c, _)| c.needs_phase2())
            .map(|(_, &h)| FstSignalHandle::from_index(h))
            .collect();
        if !p2_handles.is_empty() {
            for c in collectors.iter_mut() {
                if c.needs_phase2() {
                    c.demoted = true;
                }
            }
            let filter = FstFilter {
                start: 0,
                end: Some(p1_start),
                include: Some(p2_handles),
            };
            let read = self.reader.read_signals(&filter, |time, handle, value| {
                let t = time as i64;
                if t > from {
                    return;
                }
                let Some(&s) = slot_of.get(handle.get_index()) else {
                    return;
                };
                if s == u32::MAX {
                    return;
                }
                let c = &mut collectors[s as usize];
                if !c.demoted {
                    return;
                }
                if let Some(rv) = decode(c.kind, &value) {
                    c.seed2 = Some((t, rv)); // ascending stream: last wins
                }
            });
            if read.is_err() {
                return Vec::new();
            }
        }

        // Assemble unique traces. Collectors were created in first-encounter
        // order, so with no duplicate sids (the normal case) they are already
        // in slot order and move straight out; duplicates fall back to clones.
        let traces: Vec<SignalTrace> = collectors.into_iter().map(Collector::finish).collect();
        if traces.len() == slots.len() {
            return traces;
        }
        slots
            .iter()
            .map(|&(h, _)| {
                let t = &traces[slot_of[h] as usize];
                SignalTrace {
                    times: t.times.clone(),
                    values: t.values.clone(),
                }
            })
            .collect()
    }
}

fn empty_trace() -> SignalTrace {
    SignalTrace {
        times: Vec::new(),
        values: Vec::new(),
    }
}

/// Per-signal window reduction. Split from the reader driving so the exact
/// seed/dedup semantics are unit-testable on synthetic emissions.
struct Collector {
    kind: WinKind,
    /// Last phase-1 emission at-or-before `from` (seed candidate).
    seed: Option<(i64, RawValue)>,
    /// Seed candidate sits at a suspected frame timestamp: not a real change,
    /// so it must be re-derived by phase 2 (its value — the carried state — is
    /// still used nowhere; phase 2 supplies the true last change, or nothing
    /// if the signal never changed, matching the full decode exactly).
    seed_suspect: bool,
    /// In-window changes `(from, to]`, consecutive duplicates suppressed.
    win: Vec<(i64, RawValue)>,
    /// This signal was handed to phase 2.
    demoted: bool,
    /// Phase-2 result: true last change at-or-before `from`.
    seed2: Option<(i64, RawValue)>,
}

impl Collector {
    fn new(kind: WinKind) -> Collector {
        Collector {
            kind,
            seed: None,
            seed_suspect: false,
            win: Vec::new(),
            demoted: false,
            seed2: None,
        }
    }

    fn on_phase1(&mut self, t: i64, rv: RawValue, from: i64, to: Option<i64>, suspect: bool) {
        if t <= from {
            self.seed = Some((t, rv));
            self.seed_suspect = suspect;
        } else {
            if suspect {
                // Frame echo inside the window: not a change, drop. (Real
                // changes always sit at table times and are never suspect.)
                return;
            }
            if let Some(hi) = to {
                if t > hi {
                    return;
                }
            }
            // Mirror the full decode's duplicate suppression: a change whose
            // value equals the previous one is dropped (events excepted —
            // every event is kept). With no trustworthy baseline yet (seed
            // pending phase 2), keep it; `finish` re-checks the first entry
            // against the resolved seed.
            if self.kind != WinKind::Event {
                let baseline = self
                    .win
                    .last()
                    .map(|(_, v)| v)
                    .or_else(|| self.seed_ref().map(|(_, v)| v));
                if let Some(prev) = baseline {
                    if values_equal(prev, &rv) {
                        return;
                    }
                }
            }
            self.win.push((t, rv));
        }
    }

    /// The seed candidate usable as a dedup baseline (suspect ones excluded).
    fn seed_ref(&self) -> Option<&(i64, RawValue)> {
        if self.seed_suspect {
            None
        } else {
            self.seed.as_ref()
        }
    }

    fn needs_phase2(&self) -> bool {
        self.seed.is_none() || self.seed_suspect
    }

    fn finish(self) -> SignalTrace {
        let Collector {
            kind,
            seed,
            seed_suspect,
            mut win,
            demoted,
            seed2,
        } = self;
        let seed = if demoted {
            // Phase 2's answer replaces any suspect candidate; `None` means
            // the signal truly never changed before the window.
            seed2
        } else if seed_suspect {
            None
        } else {
            seed
        };
        // First in-window entry was kept without a baseline; with the seed now
        // resolved, apply the suppression it would have received inline.
        if demoted && kind != WinKind::Event {
            if let (Some((_, sv)), Some((_, w0))) = (&seed, win.first()) {
                if values_equal(sv, w0) {
                    win.remove(0);
                }
            }
        }
        let n = win.len() + usize::from(seed.is_some());
        let mut times = Vec::with_capacity(n);
        let mut values = Vec::with_capacity(n);
        if let Some((t, v)) = seed {
            times.push(t);
            values.push(v);
        }
        for (t, v) in win {
            times.push(t);
            values.push(v);
        }
        SignalTrace { times, values }
    }
}

/// Value equality as the full decode's duplicate check sees it: raw bytes.
/// Reals compare by bit pattern (NaN == NaN, 0.0 != -0.0), everything else by
/// canonical content.
fn values_equal(a: &RawValue, b: &RawValue) -> bool {
    match (a, b) {
        (RawValue::Real(x), RawValue::Real(y)) => x.to_bits() == y.to_bits(),
        _ => a == b,
    }
}

/// Canonical bit characters, mirroring wellen's ASCII round-trip
/// (`bit_char_to_num` -> nine-state lookup): uppercase folds to lowercase,
/// everything else unchanged. The full decode canonicalizes the same way, so
/// both paths render identical strings.
const fn canon_table() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = i as u8;
        i += 1;
    }
    t[b'X' as usize] = b'x';
    t[b'Z' as usize] = b'z';
    t[b'H' as usize] = b'h';
    t[b'U' as usize] = b'u';
    t[b'W' as usize] = b'w';
    t[b'L' as usize] = b'l';
    t
}
const CANON: [u8; 256] = canon_table();

/// Decode one raw FST emission into the same owned form the full path
/// produces. `None` drops emissions whose payload contradicts the declared
/// encoding (the full decode panics on such files; here the query simply
/// misses what could never be parsed).
fn decode(kind: WinKind, value: &FstSignalValue<'_>) -> Option<RawValue> {
    match (kind, value) {
        (WinKind::Event, _) => Some(RawValue::Event),
        (WinKind::Real, FstSignalValue::Real(f)) => Some(RawValue::Real(*f)),
        (WinKind::Str, FstSignalValue::String(bytes)) => {
            // ISO-8859-1 byte-to-char, exactly as the full decode maps it.
            Some(RawValue::Str(bytes.iter().map(|b| *b as char).collect()))
        }
        (WinKind::Bits, FstSignalValue::String(bytes)) => Some(RawValue::Bits(
            BitStr::from_ascii_iter(bytes.len(), bytes.iter().map(|b| CANON[*b as usize] as char)),
        )),
        _ => {
            debug_assert!(false, "FST payload contradicts declared encoding");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(s: &str) -> RawValue {
        RawValue::Bits(BitStr::new(s))
    }

    fn trace(c: Collector) -> Vec<(i64, String)> {
        let t = c.finish();
        t.times
            .iter()
            .zip(t.values.iter())
            .map(|(t, v)| (*t, v.raw_str().into_owned()))
            .collect()
    }

    #[test]
    fn seed_plus_window_basic() {
        let mut c = Collector::new(WinKind::Bits);
        // window [10, 30]
        c.on_phase1(2, bits("0"), 10, Some(30), false);
        c.on_phase1(8, bits("1"), 10, Some(30), false); // seed: last <= 10
        c.on_phase1(15, bits("0"), 10, Some(30), false);
        c.on_phase1(35, bits("1"), 10, Some(30), false); // beyond to: dropped
        assert_eq!(
            trace(c),
            vec![(8, "1".into()), (15, "0".into())]
        );
    }

    #[test]
    fn duplicate_value_suppressed_like_full_decode() {
        let mut c = Collector::new(WinKind::Bits);
        c.on_phase1(8, bits("1"), 10, None, false);
        c.on_phase1(15, bits("1"), 10, None, false); // same value: dropped
        c.on_phase1(20, bits("0"), 10, None, false);
        c.on_phase1(25, bits("0"), 10, None, false); // dropped
        c.on_phase1(30, bits("1"), 10, None, false);
        assert_eq!(trace(c), vec![(8, "1".into()), (20, "0".into()), (30, "1".into())]);
    }

    #[test]
    fn events_never_deduped() {
        let mut c = Collector::new(WinKind::Event);
        c.on_phase1(12, RawValue::Event, 10, None, false);
        c.on_phase1(14, RawValue::Event, 10, None, false);
        assert_eq!(trace(c).len(), 2);
    }

    #[test]
    fn seed_at_exactly_from_kept_with_true_time() {
        let mut c = Collector::new(WinKind::Bits);
        c.on_phase1(10, bits("1"), 10, Some(30), false);
        assert_eq!(trace(c), vec![(10, "1".into())]);
    }

    #[test]
    fn phase2_supplies_seed_and_first_window_dedup() {
        // Signal changed to "1" long before the window, then a redundant "1"
        // arrives in-window: full decode drops the redundant change.
        let mut c = Collector::new(WinKind::Bits);
        c.on_phase1(50, bits("1"), 40, Some(60), false); // in-window, no baseline yet
        assert!(c.needs_phase2());
        c.demoted = true;
        c.seed2 = Some((5, bits("1")));
        assert_eq!(trace(c), vec![(5, "1".into())]);
    }

    #[test]
    fn phase2_no_seed_means_absent_not_x() {
        let mut c = Collector::new(WinKind::Bits);
        c.on_phase1(50, bits("x"), 40, Some(60), false);
        assert!(c.needs_phase2());
        c.demoted = true; // phase 2 found nothing
        assert_eq!(trace(c), vec![(50, "x".into())]);
    }

    #[test]
    fn suspect_frame_seed_demoted_and_window_echo_dropped() {
        let mut c = Collector::new(WinKind::Bits);
        // Frame echo at t=8 (suspect): candidate only, not a real change.
        c.on_phase1(8, bits("1"), 10, Some(30), true);
        assert!(c.needs_phase2());
        // In-window frame echo dropped outright.
        let mut d = Collector::new(WinKind::Bits);
        d.on_phase1(12, bits("1"), 10, Some(30), true);
        d.demoted = true;
        assert_eq!(trace(d), Vec::<(i64, String)>::new());
    }

    #[test]
    fn real_dedup_by_bit_pattern() {
        let mut c = Collector::new(WinKind::Real);
        c.on_phase1(8, RawValue::Real(f64::NAN), 0, None, false);
        c.on_phase1(9, RawValue::Real(f64::NAN), 0, None, false); // same bits: dropped
        c.on_phase1(10, RawValue::Real(0.0), 0, None, false);
        c.on_phase1(11, RawValue::Real(-0.0), 0, None, false); // different bits: kept
        assert_eq!(trace(c).len(), 3);
    }

    #[test]
    fn canonicalization_folds_case() {
        let v = decode(
            WinKind::Bits,
            &FstSignalValue::String(b"1X0Z"),
        )
        .unwrap();
        assert_eq!(v.raw_str(), "1x0z");
    }

    #[test]
    fn string_latin1_decode() {
        let v = decode(WinKind::Str, &FstSignalValue::String(&[0x68, 0x69, 0xe9])).unwrap();
        assert_eq!(v.raw_str(), "hié");
    }
}
