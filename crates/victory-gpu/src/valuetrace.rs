//! What became of the bottom-of-pipe value writes the queues' waits for
//! memory values stand at (`VICTORY_TRACE_VALUES=1`, off by default; off,
//! each hook is one load of a flag that was read once).
//!
//! The title's loading screen stalls with queues standing at waits for a
//! monotonic 64-bit counter that lists write at the bottom of the pipe
//! (`crate::list` `write64`, `value_written`), and those waits are met only
//! once the flights that write it are settled, or are forced through at a
//! present. With the switch:
//!
//! - every such write is booked by address: the value, the title list and
//!   host list that wrote it, when; and when it reached title memory
//!   (`crate::queue::Written::write`);
//! - the host lists of booked writes are followed through their stages:
//!   submitted to a queue, held by it (behind what), in the line of our
//!   submitting thread, sent to the card, landed;
//! - each wait passed at a present without being met is told with all of
//!   that (per address: the first 8, then every power of two;
//!   [`Book::tell`]), and with whether the flights in the air are known to
//!   meet it (`crate::host::late_met_known`);
//! - every 5 s while a queue has stood at a wait for over a second, one line
//!   per wait it stands at ([`Book::summary`]).
//!
//! Host lists are known by their allocator (`HostList::trace_id`): the host
//! list moves when it is taken from the title's list, its allocator does not.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Whether the trace is on (`VICTORY_TRACE_VALUES=1`), read once.
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::switch::on("VICTORY_TRACE_VALUES"))
}

/// Where a host list that wrote a booked value is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    /// Recorded, not submitted.
    Recorded,
    /// Given to a queue.
    Submitted,
    /// Held by its queue, behind a wait.
    Held,
    /// In the line of our submitting thread (`crate::queue` `behind`).
    InLine,
    /// Sent to the card and waited for.
    Running,
    /// Sent to the card and not waited for (a flight).
    InFlight,
    /// Its booked write reached title memory.
    Landed,
}

impl Stage {
    fn name(self) -> &'static str {
        match self {
            Stage::Recorded => "recorded, not submitted",
            Stage::Submitted => "submitted",
            Stage::Held => "held by its queue",
            Stage::InLine => "in the submitting thread's line",
            Stage::Running => "running, waited for",
            Stage::InFlight => "in flight",
            Stage::Landed => "landed",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ListState {
    /// The queue (its address); 0 until it is known.
    queue: usize,
    stage: Stage,
    since: Instant,
}

/// The last value written at an address, as recorded.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Write {
    pub(crate) value: u64,
    pub(crate) title_list: usize,
    pub(crate) host_list: usize,
    /// Which recording of that host list ([`Book::recycled`]).
    pub(crate) generation: u64,
    pub(crate) recorded: Instant,
    /// When that value reached title memory.
    pub(crate) landed: Option<Instant>,
}

/// A wait a queue stands at: queue, address, comparison, reference, 64-bit.
pub(crate) type Key = (usize, u64, u32, u64, bool);

/// The wait's parts, as a [`Key`] on `queue`.
pub(crate) fn key(queue: usize, wait: &crate::list::ValueWait) -> Key {
    let (address, comparison, reference, wide) = wait.parts();
    (queue, address, comparison, reference, wide)
}

#[derive(Clone, Copy, Debug)]
struct Standing {
    since: Instant,
    seen: Instant,
}

/// The books: kept small, entries are dropped once there are too many.
#[derive(Default)]
pub(crate) struct Book {
    writes: HashMap<u64, Write>,
    /// The last value that reached title memory at a booked address.
    landings: HashMap<u64, (u64, Instant)>,
    /// By host list and recording: a host list is reset and used again
    /// (`crate::list::recycle`), and an allocator's address may be another
    /// one's later.
    lists: HashMap<(usize, u64), ListState>,
    /// Each followed host list's recording now; recordings are numbered
    /// once for all lists, so an old one is never taken for a new one.
    current: HashMap<usize, u64>,
    generations: u64,
    standing: HashMap<Key, Standing>,
    told: HashMap<u64, u64>,
    last_summary: Option<Instant>,
}

/// Entries kept per map before old ones go.
const MOST: usize = 4096;
/// A wait not seen standing for this long is forgotten (its queue went).
const STALE: Duration = Duration::from_secs(30);
/// How long a wait stands before the summary names it, and how often.
const HELD_LONG: Duration = Duration::from_secs(1);
const EVERY: Duration = Duration::from_secs(5);

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

impl Book {
    /// A list recorded a bottom-of-pipe write of `value` at `address`.
    pub(crate) fn wrote(
        &mut self,
        address: u64,
        value: u64,
        title_list: usize,
        host_list: usize,
        now: Instant,
    ) {
        if self.writes.len() >= MOST && !self.writes.contains_key(&address) {
            self.writes.clear();
            self.landings.clear();
        }
        let generation = if host_list == 0 {
            0
        } else {
            let mut generation = self.current.get(&host_list).copied();
            // A list that went further than recorded records again: another
            // recording (its address taken by another allocator, say).
            if let Some(g) = generation
                && self
                    .lists
                    .get(&(host_list, g))
                    .is_some_and(|l| l.stage != Stage::Recorded)
            {
                generation = None;
            }
            let generation = generation.unwrap_or_else(|| self.fresh(host_list));
            if self.lists.len() >= MOST && !self.lists.contains_key(&(host_list, generation)) {
                self.lists.retain(|_, l| l.stage != Stage::Landed);
                if self.lists.len() >= MOST {
                    self.lists.clear();
                }
                let lists = &self.lists;
                self.current.retain(|id, g| lists.contains_key(&(*id, *g)));
                self.current.insert(host_list, generation);
            }
            self.lists
                .entry((host_list, generation))
                .or_insert(ListState {
                    queue: 0,
                    stage: Stage::Recorded,
                    since: now,
                });
            generation
        };
        self.writes.insert(
            address,
            Write {
                value,
                title_list,
                host_list,
                generation,
                recorded: now,
                landed: None,
            },
        );
    }

    /// A new recording of host list `id`.
    fn fresh(&mut self, id: usize) -> u64 {
        self.generations += 1;
        self.current.insert(id, self.generations);
        self.generations
    }

    /// Host list `id` was reset to be used again: what it is staged as from
    /// now on is not the booked recording's.
    pub(crate) fn recycled(&mut self, id: usize) {
        self.current.remove(&id);
    }

    /// The host lists `ids` moved to `stage` (on `queue`, if it is given).
    /// Only lists with a booked write are followed.
    pub(crate) fn staged(
        &mut self,
        ids: impl IntoIterator<Item = usize>,
        queue: Option<usize>,
        stage: Stage,
        now: Instant,
    ) {
        for id in ids {
            let Some(&generation) = self.current.get(&id) else {
                continue;
            };
            if let Some(list) = self.lists.get_mut(&(id, generation)) {
                if let Some(queue) = queue {
                    list.queue = queue;
                }
                if list.stage != stage {
                    list.stage = stage;
                    list.since = now;
                }
            }
        }
    }

    /// `value` (its low `bytes`) reached title memory at `address`.
    pub(crate) fn landed(&mut self, address: u64, bytes: usize, value: u64, now: Instant) {
        let Some(write) = self.writes.get_mut(&address) else {
            return;
        };
        let mask = if bytes >= 8 {
            u64::MAX
        } else {
            (1u64 << (bytes * 8)) - 1
        };
        self.landings.insert(address, (value & mask, now));
        if write.landed.is_none() && write.value & mask == value & mask {
            write.landed = Some(now);
            if let Some(list) = self.lists.get_mut(&(write.host_list, write.generation))
                && list.stage != Stage::Landed
            {
                list.stage = Stage::Landed;
                list.since = now;
            }
        }
    }

    /// A queue stands at the wait `key`.
    pub(crate) fn stands(&mut self, key: Key, now: Instant) {
        if self.standing.len() >= MOST && !self.standing.contains_key(&key) {
            self.standing
                .retain(|_, s| now.duration_since(s.seen) < STALE);
            // Still full: the one seen longest ago goes.
            while self.standing.len() >= MOST {
                let Some(oldest) = self
                    .standing
                    .iter()
                    .min_by_key(|(_, s)| s.seen)
                    .map(|(k, _)| *k)
                else {
                    break;
                };
                self.standing.remove(&oldest);
            }
        }
        self.standing
            .entry(key)
            .or_insert(Standing {
                since: now,
                seen: now,
            })
            .seen = now;
    }

    /// The queue went past the wait `key`.
    pub(crate) fn passes(&mut self, key: &Key) {
        self.standing.remove(key);
    }

    /// Whether to tell of another unmet wait at `address`: the first 8,
    /// then the 16th, 32nd, 64th...; and how many there were.
    pub(crate) fn tell(&mut self, address: u64) -> (bool, u64) {
        if self.told.len() >= MOST && !self.told.contains_key(&address) {
            self.told.clear();
        }
        let n = self.told.entry(address).or_insert(0);
        *n += 1;
        (*n <= 8 || n.is_power_of_two(), *n)
    }

    /// Every [`EVERY`] while a queue has stood at a wait for over
    /// [`HELD_LONG`]: each wait standing, the longest first, with how long.
    pub(crate) fn summary(&mut self, now: Instant) -> Option<Vec<(Key, Duration)>> {
        self.standing
            .retain(|_, s| now.duration_since(s.seen) < STALE);
        if self
            .last_summary
            .is_some_and(|last| now.duration_since(last) < EVERY)
        {
            return None;
        }
        if !self
            .standing
            .values()
            .any(|s| now.duration_since(s.since) > HELD_LONG)
        {
            return None;
        }
        self.last_summary = Some(now);
        let mut all: Vec<(Key, Duration)> = self
            .standing
            .iter()
            .map(|(k, s)| (*k, now.duration_since(s.since)))
            .collect();
        all.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        Some(all)
    }

    /// The last write booked at `address`.
    pub(crate) fn write(&self, address: u64) -> Option<Write> {
        self.writes.get(&address).copied()
    }

    /// The stage of host list `id`'s recording now and the queue it is on.
    #[cfg(test)]
    pub(crate) fn stage(&self, id: usize) -> Option<(Stage, usize)> {
        let generation = self.current.get(&id)?;
        self.lists
            .get(&(id, *generation))
            .map(|l| (l.stage, l.queue))
    }

    /// The stage of the list that made the last write booked at `address`,
    /// and the queue it is on.
    pub(crate) fn stage_of_write(&self, address: u64) -> Option<(Stage, usize)> {
        let write = self.writes.get(&address)?;
        self.lists
            .get(&(write.host_list, write.generation))
            .map(|l| (l.stage, l.queue))
    }

    /// What is known of the last write at `address`, in words; `describe`
    /// tells what a queue holds first.
    pub(crate) fn story(
        &self,
        address: u64,
        now: Instant,
        describe: &dyn Fn(usize) -> String,
    ) -> String {
        let Some(write) = self.writes.get(&address) else {
            return "no write of it was recorded".into();
        };
        let mut s = format!(
            "the last write recorded: {:#x} by list {:#x} (host list {:#x}), {:.1} ms ago, ",
            write.value,
            write.title_list,
            write.host_list,
            ms(now.duration_since(write.recorded))
        );
        match write.landed {
            Some(at) => s += &format!("landed {:.1} ms ago", ms(now.duration_since(at))),
            None => s += "not landed",
        }
        if let Some(&(value, at)) = self.landings.get(&address) {
            s += &format!(
                "; the last value landed there: {value:#x}, {:.1} ms ago",
                ms(now.duration_since(at))
            );
        }
        match self.lists.get(&(write.host_list, write.generation)) {
            Some(list) => {
                s += &format!(
                    "; its list: {} for {:.1} ms",
                    list.stage.name(),
                    ms(now.duration_since(list.since))
                );
                if list.queue != 0 {
                    s += &format!(" on queue {:#x}", list.queue);
                }
                if matches!(list.stage, Stage::Held | Stage::InLine | Stage::Submitted)
                    && list.queue != 0
                {
                    s += &format!(" (that queue stands at: {})", describe(list.queue));
                }
            }
            None => s += "; its list is not followed",
        }
        s
    }
}

fn book() -> std::sync::MutexGuard<'static, Book> {
    static BOOK: OnceLock<Mutex<Book>> = OnceLock::new();
    BOOK.get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// [`Book::wrote`], if the trace is on.
pub(crate) fn wrote(address: u64, value: u64, title_list: usize, host_list: usize) {
    if on() {
        book().wrote(address, value, title_list, host_list, Instant::now());
    }
}

/// [`Book::staged`], if the trace is on.
pub(crate) fn staged(ids: impl IntoIterator<Item = usize>, queue: Option<usize>, stage: Stage) {
    if on() {
        book().staged(ids, queue, stage, Instant::now());
    }
}

/// [`Book::recycled`], if the trace is on.
pub(crate) fn recycled(id: usize) {
    if on() {
        book().recycled(id);
    }
}

/// [`Book::landed`], if the trace is on.
pub(crate) fn landed(address: u64, bytes: usize, value: u64) {
    if on() {
        book().landed(address, bytes, value, Instant::now());
    }
}

/// `queue` stands at `wait` (`stands`) or goes past it.
pub(crate) fn standing(queue: usize, wait: &crate::list::ValueWait, stands: bool) {
    if !on() {
        return;
    }
    let key = key(queue, wait);
    if stands {
        book().stands(key, Instant::now());
        summaries();
    } else {
        book().passes(&key);
    }
}

/// The value at the wait's address now, if it can be read.
fn read(address: u64, wide: bool) -> Option<u64> {
    #[cfg(windows)]
    {
        let bytes = if wide { 8 } else { 4 };
        let mut raw = [0u8; 8];
        (address != 0 && crate::host::read_into(address as *const u8, &mut raw[..bytes]))
            .then(|| u64::from_le_bytes(raw))
    }
    #[cfg(not(windows))]
    {
        let _ = (address, wide);
        None
    }
}

fn shown(value: Option<u64>) -> String {
    value.map_or_else(|| "unreadable".into(), |v| format!("{v:#x}"))
}

/// The line told of a wait passed at a present without being met (pure:
/// for the tests).
pub(crate) fn unmet_line(
    book: &Book,
    key: Key,
    now_value: Option<u64>,
    late_known: Option<Option<bool>>,
    told: u64,
    now: Instant,
    describe: &dyn Fn(usize) -> String,
) -> String {
    let (queue, address, comparison, reference, wide) = key;
    let late = match late_known {
        None => "no flights (not sent ahead)".to_string(),
        Some(None) => "the flights in the air may meet it (not known without a wait)".into(),
        Some(Some(true)) => "a flight in the air meets it".into(),
        Some(Some(false)) => "no flight in the air meets it".into(),
    };
    format!(
        "queue {queue:#x} at {address:#x} (comparison {comparison} reference {reference:#x}, {} bits): memory holds {}; {}; {late}; unmet wait {told} at this address",
        if wide { 64 } else { 32 },
        shown(now_value),
        book.story(address, now, describe),
    )
}

/// A wait passed at a present without being met: told (per address: the
/// first 8, then every power of two), with what the books know if the
/// trace is on. `describe` tells what a queue stands at first.
pub(crate) fn unmet_at_present(
    queue: usize,
    wait: &crate::list::ValueWait,
    describe: &dyn Fn(usize) -> String,
) {
    let key = key(queue, wait);
    let (tell, told) = book().tell(key.1);
    if !on() {
        if tell {
            tracing::warn!(
                queue = format_args!("{queue:#x}"),
                ?wait,
                told,
                "a wait for a memory value was not met at a present: the queue runs on"
            );
        }
        return;
    }
    book().passes(&key);
    if !tell {
        return;
    }
    let now_value = read(key.1, key.4);
    #[cfg(windows)]
    let late_known = crate::host::late().then(|| crate::host::late_met_known(wait));
    #[cfg(not(windows))]
    let late_known = None;
    let now = Instant::now();
    // (`describe` is asked outside the books' lock: it looks at others.)
    let held_queue = {
        let b = book();
        b.stage_of_write(key.1).map(|(_, q)| q)
    };
    let described = held_queue.filter(|&q| q != 0).map(describe);
    let line = {
        let b = book();
        unmet_line(&b, key, now_value, late_known, told, now, &|_| {
            described.clone().unwrap_or_default()
        })
    };
    tracing::warn!("a wait for a memory value was not met at a present: the queue runs on: {line}");
}

/// Starts the thread that tells the summary ([`Book::summary`]), once.
fn summaries() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let _ = std::thread::Builder::new().name("victory-value-trace".into()).spawn(|| {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let Some(held) = book().summary(Instant::now()) else {
                    continue;
                };
                for ((queue, address, comparison, reference, wide), age) in held {
                    let last = book().write(address).map(|w| w.value);
                    tracing::warn!(
                        "a queue stands at a wait for a memory value: queue {queue:#x} at {address:#x} (comparison {comparison} reference {reference:#x}), memory holds {}, the last write recorded {}, for {:.1} s",
                        shown(read(address, wide)),
                        shown(last),
                        age.as_secs_f64()
                    );
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: u64 = 0x1_2750_0000;

    #[test]
    fn a_write_is_booked_then_landed_with_its_list() {
        let t = Instant::now();
        let mut b = Book::default();
        b.wrote(AT, 41, 0x10, 0x20, t);
        assert_eq!(b.stage(0x20), Some((Stage::Recorded, 0)));
        b.staged([0x20, 0x99], Some(0x30), Stage::Submitted, t);
        b.staged([0x20], None, Stage::InFlight, t);
        assert_eq!(b.stage(0x20), Some((Stage::InFlight, 0x30)));
        // A list not booked is not followed.
        assert_eq!(b.stage(0x99), None);
        // An older value landing is not the booked one.
        b.landed(AT, 8, 40, t);
        assert!(b.write(AT).unwrap().landed.is_none());
        assert_eq!(b.stage(0x20), Some((Stage::InFlight, 0x30)));
        b.landed(AT, 8, 41, t + Duration::from_millis(3));
        assert_eq!(
            b.write(AT).unwrap().landed,
            Some(t + Duration::from_millis(3))
        );
        assert_eq!(b.stage(0x20), Some((Stage::Landed, 0x30)));
    }

    #[test]
    fn a_recycled_list_does_not_move_the_old_write() {
        let t = Instant::now();
        let mut b = Book::default();
        b.wrote(AT, 1, 0x10, 0x20, t);
        b.staged([0x20], Some(0x30), Stage::InFlight, t);
        b.recycled(0x20);
        // Submitted again without a booked write: not the old recording.
        b.staged([0x20], Some(0x31), Stage::Held, t);
        assert_eq!(b.stage_of_write(AT), Some((Stage::InFlight, 0x30)));
        assert_eq!(b.stage(0x20), None);
        // Its next write is a recording of its own.
        b.wrote(AT + 8, 2, 0x11, 0x20, t);
        b.staged([0x20], Some(0x31), Stage::Held, t);
        assert_eq!(b.stage_of_write(AT + 8), Some((Stage::Held, 0x31)));
        assert_eq!(b.stage_of_write(AT), Some((Stage::InFlight, 0x30)));
    }

    #[test]
    fn an_allocator_address_used_again_is_another_recording() {
        let t = Instant::now();
        let mut b = Book::default();
        b.wrote(AT, 1, 0x10, 0x20, t);
        b.staged([0x20], Some(0x30), Stage::InFlight, t);
        // Not recycled (freed, the address given to a new allocator): a
        // write after the list went further starts another recording.
        b.wrote(AT + 8, 2, 0x11, 0x20, t);
        assert_eq!(b.stage_of_write(AT + 8), Some((Stage::Recorded, 0)));
        assert_eq!(b.stage_of_write(AT), Some((Stage::InFlight, 0x30)));
        // Two writes of one recording share it.
        b.wrote(AT + 16, 3, 0x11, 0x20, t);
        b.staged([0x20], Some(0x32), Stage::Submitted, t);
        assert_eq!(b.stage_of_write(AT + 8), Some((Stage::Submitted, 0x32)));
        assert_eq!(b.stage_of_write(AT + 16), Some((Stage::Submitted, 0x32)));
    }

    #[test]
    fn the_standing_waits_are_capped_oldest_first() {
        let t = Instant::now();
        let mut b = Book::default();
        for i in 0..(MOST as u64 + 5) {
            b.stands((1, i * 8, 7, 9, true), t + Duration::from_millis(i));
        }
        assert_eq!(b.standing.len(), MOST);
        assert!(!b.standing.contains_key(&(1, 0, 7, 9, true)));
        assert!(
            b.standing
                .contains_key(&(1, (MOST as u64 + 4) * 8, 7, 9, true))
        );
    }

    #[test]
    fn a_32_bit_landing_compares_the_low_half() {
        let t = Instant::now();
        let mut b = Book::default();
        b.wrote(AT, 0x5, 1, 2, t);
        b.landed(AT, 4, 0xFFFF_FFFF_0000_0005, t);
        assert!(b.write(AT).unwrap().landed.is_some());
    }

    #[test]
    fn a_landing_at_an_address_not_booked_is_ignored() {
        let t = Instant::now();
        let mut b = Book::default();
        b.landed(AT, 8, 1, t);
        assert!(b.write(AT).is_none());
        assert!(b.story(AT, t, &|_| String::new()).contains("no write"));
    }

    #[test]
    fn unmet_waits_are_told_first_8_then_powers_of_two() {
        let mut b = Book::default();
        let told: Vec<u64> = (0..100)
            .filter_map(|_| match b.tell(AT) {
                (true, n) => Some(n),
                _ => None,
            })
            .collect();
        assert_eq!(told, [1, 2, 3, 4, 5, 6, 7, 8, 16, 32, 64]);
        // Per address: another one starts again.
        assert_eq!(b.tell(AT + 8), (true, 1));
    }

    #[test]
    fn the_summary_waits_for_a_second_and_comes_every_5_s() {
        let t = Instant::now();
        let mut b = Book::default();
        let k: Key = (0x30, AT, 7, 9, true);
        b.stands(k, t);
        assert!(b.summary(t + Duration::from_millis(900)).is_none());
        // Seen again: it stands since the first time.
        b.stands(k, t + Duration::from_millis(1000));
        let s = b.summary(t + Duration::from_millis(1100)).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0], (k, Duration::from_millis(1100)));
        assert!(b.summary(t + Duration::from_secs(5)).is_none());
        b.stands(k, t + Duration::from_secs(6));
        assert!(b.summary(t + Duration::from_millis(6200)).is_some());
        b.passes(&k);
        assert!(b.summary(t + Duration::from_secs(20)).is_none());
    }

    #[test]
    fn a_wait_no_longer_seen_is_forgotten() {
        let t = Instant::now();
        let mut b = Book::default();
        b.stands((1, AT, 7, 9, true), t);
        assert!(b.summary(t + STALE + Duration::from_secs(1)).is_none());
    }

    #[test]
    fn the_unmet_line_says_where_the_write_is() {
        let t = Instant::now();
        let mut b = Book::default();
        b.wrote(AT, 0x2a, 0x10, 0x20, t);
        b.staged([0x20], Some(0x30), Stage::Held, t);
        let k: Key = (0x40, AT, 7, 0x2a, true);
        let line = unmet_line(
            &b,
            k,
            Some(0x29),
            Some(Some(false)),
            3,
            t + Duration::from_millis(5),
            &|q| format!("head of {q:#x}"),
        );
        for part in [
            "queue 0x40 at 0x127500000",
            "reference 0x2a, 64 bits",
            "memory holds 0x29",
            "the last write recorded: 0x2a by list 0x10 (host list 0x20), 5.0 ms ago, not landed",
            "its list: held by its queue for 5.0 ms on queue 0x30 (that queue stands at: head of 0x30)",
            "no flight in the air meets it",
            "unmet wait 3",
        ] {
            assert!(line.contains(part), "{part:?} not in {line:?}");
        }
        let line = unmet_line(&b, k, None, None, 1, t, &|_| String::new());
        assert!(
            line.contains("memory holds unreadable") && line.contains("not sent ahead"),
            "{line}"
        );
    }

    #[test]
    fn a_full_book_starts_again_rather_than_growing() {
        let t = Instant::now();
        let mut b = Book::default();
        for i in 0..(MOST as u64 + 10) {
            b.wrote(i * 8, i, 1, 0x1000 + i as usize, t);
        }
        assert!(b.writes.len() <= MOST && b.lists.len() <= MOST);
        assert!(b.write((MOST as u64 + 9) * 8).is_some());
    }
}
