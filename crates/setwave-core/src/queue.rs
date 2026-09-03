//! Playback order.
//!
//! Deliberately free of any engine or storage concern: the queue holds track ids
//! and answers "what plays next", which makes every rule here directly testable.

/// Deterministic, seedable PRNG for shuffle.
///
/// A real dependency would be overkill for one use, and seeding makes shuffle
/// behaviour reproducible in tests rather than something we hope is uniform.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Any nonzero state; xorshift64* degenerates from 0.
        Self(seed.max(1) ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepeatMode {
    #[default]
    Off,
    /// Wrap around at the end of the queue.
    All,
    /// Keep replaying the current track.
    One,
}

impl RepeatMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            RepeatMode::Off => "off",
            RepeatMode::All => "all",
            RepeatMode::One => "one",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" => Some(RepeatMode::Off),
            "all" | "queue" => Some(RepeatMode::All),
            "one" | "track" | "single" => Some(RepeatMode::One),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Queue {
    items: Vec<i64>,
    /// Index into `items` of what is playing. `None` when nothing is selected.
    current: Option<usize>,
    repeat: RepeatMode,
    shuffle: bool,
    /// Playback order when shuffling: a permutation of indices into `items`.
    order: Vec<usize>,
    seed: u64,
    /// Positions playback is confined to, sorted and deduplicated. `None`
    /// walks the whole queue. Positions rather than ids: a queue may hold the
    /// same track twice, and the user chose rows, not tracks.
    loop_set: Option<Vec<usize>>,
}

impl Queue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn items(&self) -> &[i64] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn repeat(&self) -> RepeatMode {
        self.repeat
    }

    pub fn set_repeat(&mut self, mode: RepeatMode) {
        self.repeat = mode;
        self.loop_set = None;
    }

    pub fn shuffle_enabled(&self) -> bool {
        self.shuffle
    }

    pub fn loop_set(&self) -> Option<&[usize]> {
        self.loop_set.as_deref()
    }

    /// Confine playback to `positions` — or lift the confinement with `None`.
    ///
    /// Exclusive with repeat: a loop is its own answer to "what happens at the
    /// end", so setting one puts repeat back to off, and `set_repeat` clears
    /// the loop in turn. Out-of-range positions are dropped; an empty set is
    /// the same as none.
    pub fn set_loop(&mut self, positions: Option<Vec<usize>>) {
        self.loop_set = positions
            .map(|mut p| {
                p.retain(|i| *i < self.items.len());
                p.sort_unstable();
                p.dedup();
                p
            })
            .filter(|p| !p.is_empty());
        if self.loop_set.is_some() {
            self.repeat = RepeatMode::Off;
        }
    }

    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    pub fn current(&self) -> Option<i64> {
        self.current.and_then(|i| self.items.get(i)).copied()
    }

    /// Replace the queue, selecting `start` (an index into `tracks`).
    pub fn replace(&mut self, tracks: Vec<i64>, start: Option<usize>) {
        self.items = tracks;
        self.current = start.filter(|i| *i < self.items.len());
        self.loop_set = None;
        self.reshuffle();
    }

    /// Append to the end, keeping whatever is playing.
    pub fn extend(&mut self, tracks: impl IntoIterator<Item = i64>) {
        let before = self.items.len();
        self.items.extend(tracks);
        if self.items.len() != before {
            self.reshuffle();
        }
    }

    /// Insert at the top — the stage — keeping whatever is playing.
    ///
    /// The current index moves with its track rather than staying put: staging
    /// something must never silently reassign "what is playing" to a different
    /// file.
    pub fn insert_front(&mut self, tracks: impl IntoIterator<Item = i64>) {
        let staged: Vec<i64> = tracks.into_iter().collect();
        if staged.is_empty() {
            return;
        }
        let count = staged.len();
        self.items.splice(0..0, staged);
        self.current = self.current.map(|i| i + count);
        if let Some(set) = self.loop_set.as_mut() {
            for position in set.iter_mut() {
                *position += count;
            }
        }
        self.reshuffle();
    }

    /// Move one item, returning whether anything changed.
    ///
    /// Indices are into the queue as displayed, which is the unshuffled order:
    /// reordering is about the list the user is looking at, and shuffle is
    /// layered over it.
    pub fn move_item(&mut self, from: usize, to: usize) -> bool {
        if from >= self.items.len() || to >= self.items.len() || from == to {
            return false;
        }
        let id = self.items.remove(from);
        self.items.insert(to, id);

        // Follow the current track and the loop through the move by index
        // arithmetic rather than by id: a queue may hold the same track twice.
        self.current = self.current.map(|c| Self::position_after_move(c, from, to));
        if let Some(set) = self.loop_set.as_mut() {
            for position in set.iter_mut() {
                *position = Self::position_after_move(*position, from, to);
            }
            set.sort_unstable();
        }
        self.reshuffle();
        true
    }

    /// Where the item at `position` sits once `from` has moved to `to`.
    fn position_after_move(position: usize, from: usize, to: usize) -> usize {
        if position == from {
            return to;
        }
        let mut moved = position;
        if moved > from {
            moved -= 1;
        }
        if moved >= to {
            moved += 1;
        }
        moved
    }

    /// Permute the tracks at `positions` among themselves — or the whole queue
    /// with `None`. A one-shot, visible reorder, unlike shuffle mode, which
    /// leaves the list alone and hides the order it actually plays in.
    ///
    /// The loop, being positional, is untouched: scrambling five selected rows
    /// and then looping them loops the same five slots.
    pub fn scramble(&mut self, positions: Option<&[usize]>) {
        let slots: Vec<usize> = match positions {
            Some(chosen) => {
                let mut slots: Vec<usize> = chosen
                    .iter()
                    .copied()
                    .filter(|i| *i < self.items.len())
                    .collect();
                slots.sort_unstable();
                slots.dedup();
                slots
            }
            None => (0..self.items.len()).collect(),
        };
        if slots.len() < 2 {
            return;
        }

        let mut rng = Rng::new(self.seed_or_clock());
        let mut perm: Vec<usize> = (0..slots.len()).collect();
        for i in (1..perm.len()).rev() {
            let j = rng.below(i + 1);
            perm.swap(i, j);
        }

        let before: Vec<i64> = slots.iter().map(|&s| self.items[s]).collect();
        for (j, &slot) in slots.iter().enumerate() {
            self.items[slot] = before[perm[j]];
        }
        // The current track went from slot index k to the j with perm[j] == k.
        if let Some(cur) = self.current {
            if let Some(k) = slots.iter().position(|&s| s == cur) {
                if let Some(j) = perm.iter().position(|&p| p == k) {
                    self.current = Some(slots[j]);
                }
            }
        }
        self.reshuffle();
    }

    fn seed_or_clock(&self) -> u64 {
        if self.seed == 0 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x5EED)
        } else {
            self.seed
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.order.clear();
        self.current = None;
        self.loop_set = None;
    }

    /// Select `track_id` if present, so "play this" from a list keeps context.
    pub fn select_track(&mut self, track_id: i64) -> bool {
        match self.items.iter().position(|t| *t == track_id) {
            Some(i) => {
                self.current = Some(i);
                true
            }
            None => false,
        }
    }

    pub fn set_shuffle(&mut self, on: bool) {
        if self.shuffle != on {
            self.shuffle = on;
            self.reshuffle();
        }
    }

    /// Seed the shuffle. Exposed so tests are deterministic.
    pub fn set_seed(&mut self, seed: u64) {
        self.seed = seed;
        self.reshuffle();
    }

    /// Rebuild the shuffled order, keeping the current track first so enabling
    /// shuffle mid-track does not skip what is playing.
    fn reshuffle(&mut self) {
        self.order = (0..self.items.len()).collect();
        if !self.shuffle || self.items.len() < 2 {
            return;
        }
        let mut rng = Rng::new(self.seed_or_clock());
        // Fisher-Yates.
        for i in (1..self.order.len()).rev() {
            let j = rng.below(i + 1);
            self.order.swap(i, j);
        }
        if let Some(cur) = self.current {
            if let Some(pos) = self.order.iter().position(|i| *i == cur) {
                self.order.swap(0, pos);
            }
        }
    }

    /// Position of the current track within the playback order.
    fn order_position(&self) -> Option<usize> {
        let cur = self.current?;
        self.order.iter().position(|i| *i == cur)
    }

    /// Advance to the next track, honouring shuffle and repeat.
    ///
    /// `RepeatMode::One` is deliberately ignored here: an explicit "next" from
    /// the user means the next track, not the same one again. Automatic advance
    /// at end-of-file goes through [`Queue::advance_on_eof`] instead.
    pub fn next(&mut self) -> Option<i64> {
        if self.items.is_empty() {
            return None;
        }
        if let Some(set) = &self.loop_set {
            // A loop always wraps; that is what makes it a loop. Shuffle mode's
            // order is ignored inside one — the user picked these rows in
            // this order.
            let at = self.current.and_then(|c| set.iter().position(|&p| p == c));
            self.current = Some(match at {
                Some(k) => set[(k + 1) % set.len()],
                None => set[0],
            });
            return self.current();
        }
        let Some(pos) = self.order_position() else {
            self.current = self.order.first().copied();
            return self.current();
        };
        let next_pos = pos + 1;
        if next_pos < self.order.len() {
            self.current = self.order.get(next_pos).copied();
        } else if self.repeat == RepeatMode::All {
            self.current = self.order.first().copied();
        } else {
            return None;
        }
        self.current()
    }

    pub fn previous(&mut self) -> Option<i64> {
        if self.items.is_empty() {
            return None;
        }
        if let Some(set) = &self.loop_set {
            let at = self.current.and_then(|c| set.iter().position(|&p| p == c));
            self.current = Some(match at {
                Some(k) => set[(k + set.len() - 1) % set.len()],
                None => set[set.len() - 1],
            });
            return self.current();
        }
        let pos = self.order_position()?;
        if pos > 0 {
            self.current = self.order.get(pos - 1).copied();
        } else if self.repeat == RepeatMode::All {
            self.current = self.order.last().copied();
        } else {
            return None;
        }
        self.current()
    }

    /// What to play when the current track ends by itself.
    pub fn advance_on_eof(&mut self) -> Option<i64> {
        match self.repeat {
            RepeatMode::One => self.current(),
            _ => self.next(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue_of(n: i64) -> Queue {
        let mut q = Queue::new();
        q.replace((1..=n).collect(), Some(0));
        q
    }

    #[test]
    fn walks_forwards_and_stops_at_the_end() {
        let mut q = queue_of(3);
        assert_eq!(q.current(), Some(1));
        assert_eq!(q.next(), Some(2));
        assert_eq!(q.next(), Some(3));
        assert_eq!(q.next(), None, "repeat off stops at the end");
        assert_eq!(q.current(), Some(3), "and stays on the last track");
    }

    #[test]
    fn repeat_all_wraps_in_both_directions() {
        let mut q = queue_of(3);
        q.set_repeat(RepeatMode::All);
        q.next();
        q.next();
        assert_eq!(q.next(), Some(1), "wraps forwards");
        assert_eq!(q.previous(), Some(3), "wraps backwards");
    }

    #[test]
    fn repeat_one_replays_on_eof_but_not_on_explicit_next() {
        let mut q = queue_of(3);
        q.set_repeat(RepeatMode::One);
        assert_eq!(q.advance_on_eof(), Some(1), "eof replays the same track");
        assert_eq!(q.next(), Some(2), "an explicit next still moves on");
    }

    #[test]
    fn eof_at_the_end_of_a_finite_queue_stops() {
        let mut q = queue_of(2);
        q.next();
        assert_eq!(q.advance_on_eof(), None);
    }

    #[test]
    fn previous_stops_at_the_start_without_repeat() {
        let mut q = queue_of(3);
        assert_eq!(q.previous(), None);
        assert_eq!(q.current(), Some(1));
    }

    #[test]
    fn shuffle_is_a_permutation_that_starts_from_the_current_track() {
        let mut q = queue_of(8);
        q.select_track(5);
        q.set_shuffle(true);
        q.set_seed(42);

        let mut seen = vec![q.current().unwrap()];
        while let Some(t) = q.next() {
            seen.push(t);
        }
        assert_eq!(seen[0], 5, "shuffle keeps playing the current track first");
        let mut sorted = seen.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            (1..=8).collect::<Vec<_>>(),
            "every track exactly once"
        );
    }

    #[test]
    fn shuffle_seed_is_reproducible() {
        let run = |seed| {
            let mut q = queue_of(10);
            q.set_shuffle(true);
            q.set_seed(seed);
            let mut out = vec![q.current().unwrap()];
            while let Some(t) = q.next() {
                out.push(t);
            }
            out
        };
        assert_eq!(run(7), run(7));
        assert_ne!(run(7), run(8), "different seeds give different orders");
    }

    #[test]
    fn extend_keeps_the_current_track() {
        let mut q = queue_of(2);
        q.next();
        assert_eq!(q.current(), Some(2));
        q.extend([3, 4]);
        assert_eq!(q.current(), Some(2), "adding to the queue must not skip");
        assert_eq!(q.next(), Some(3));
    }

    #[test]
    fn staging_goes_to_the_top_without_changing_what_plays() {
        let mut q = queue_of(3);
        q.next();
        assert_eq!(q.current(), Some(2));
        q.insert_front([90, 91]);
        assert_eq!(q.items(), [90, 91, 1, 2, 3]);
        assert_eq!(q.current(), Some(2), "the playing track must not change");
        assert_eq!(q.current_index(), Some(3));
    }

    #[test]
    fn staging_onto_an_idle_queue_selects_nothing() {
        let mut q = Queue::new();
        q.insert_front([7, 8]);
        assert_eq!(q.items(), [7, 8]);
        assert_eq!(q.current(), None, "staging is not playing");
    }

    #[test]
    fn moving_an_item_carries_the_current_track_with_it() {
        let mut q = queue_of(4);
        q.next();
        q.next();
        assert_eq!(q.current(), Some(3));

        assert!(q.move_item(2, 0), "the playing track moved to the top");
        assert_eq!(q.items(), [3, 1, 2, 4]);
        assert_eq!(q.current_index(), Some(0));
        assert_eq!(q.current(), Some(3));

        assert!(q.move_item(3, 1), "an idle track moved above it");
        assert_eq!(q.items(), [3, 4, 1, 2]);
        assert_eq!(q.current(), Some(3), "still the same track playing");
    }

    #[test]
    fn moving_nowhere_is_refused() {
        let mut q = queue_of(2);
        assert!(!q.move_item(0, 0));
        assert!(!q.move_item(0, 9));
        assert!(!q.move_item(9, 0));
        assert_eq!(q.items(), [1, 2]);
    }

    #[test]
    fn scramble_permutes_only_the_chosen_slots() {
        let mut q = queue_of(8);
        q.set_seed(3);
        q.scramble(Some(&[1, 3, 5, 7]));
        let items = q.items().to_vec();
        assert_eq!(
            [items[0], items[2], items[4], items[6]],
            [1, 3, 5, 7],
            "untouched slots"
        );
        let mut chosen = vec![items[1], items[3], items[5], items[7]];
        chosen.sort_unstable();
        assert_eq!(
            chosen,
            [2, 4, 6, 8],
            "chosen tracks stay in the chosen slots"
        );
    }

    #[test]
    fn scramble_follows_the_current_track() {
        let mut q = queue_of(10);
        q.select_track(4);
        for seed in 1..20 {
            q.set_seed(seed);
            q.scramble(None);
            assert_eq!(
                q.current(),
                Some(4),
                "seed {seed}: current must follow its track"
            );
            assert_eq!(q.items()[q.current_index().unwrap()], 4);
        }
        let mut all = q.items().to_vec();
        all.sort_unstable();
        assert_eq!(all, (1..=10).collect::<Vec<_>>());
    }

    #[test]
    fn scramble_with_fewer_than_two_slots_is_a_no_op() {
        let mut q = queue_of(3);
        q.scramble(Some(&[1]));
        q.scramble(Some(&[7, 9]));
        assert_eq!(q.items(), [1, 2, 3]);
    }

    #[test]
    fn a_loop_confines_next_and_previous_to_its_slots_and_wraps() {
        let mut q = queue_of(6);
        q.set_loop(Some(vec![4, 1, 4, 99]));
        assert_eq!(
            q.loop_set(),
            Some(&[1, 4][..]),
            "sorted, deduplicated, in range"
        );

        assert_eq!(
            q.next(),
            Some(2),
            "from outside the loop, enter at its first slot"
        );
        assert_eq!(q.next(), Some(5));
        assert_eq!(q.next(), Some(2), "wraps regardless of repeat mode");
        assert_eq!(q.previous(), Some(5), "wraps backwards too");
        assert_eq!(q.advance_on_eof(), Some(2), "eof stays inside the loop");
    }

    #[test]
    fn loop_and_repeat_are_exclusive() {
        let mut q = queue_of(3);
        q.set_repeat(RepeatMode::One);
        q.set_loop(Some(vec![0, 1]));
        assert_eq!(q.repeat(), RepeatMode::Off, "a loop replaces repeat");
        q.set_repeat(RepeatMode::All);
        assert_eq!(q.loop_set(), None, "and repeat replaces the loop");
        q.set_loop(Some(vec![]));
        assert_eq!(q.loop_set(), None, "an empty loop is no loop");
    }

    #[test]
    fn the_loop_follows_its_rows_through_staging_and_moves() {
        let mut q = queue_of(5);
        q.set_loop(Some(vec![1, 3]));
        q.insert_front([90, 91]);
        assert_eq!(q.loop_set(), Some(&[3, 5][..]), "pushed down by the stage");
        assert!(q.move_item(0, 6));
        assert_eq!(
            q.loop_set(),
            Some(&[2, 4][..]),
            "pulled up when a row above leaves"
        );
        assert_eq!(
            [q.items()[2], q.items()[4]],
            [2, 4],
            "still the same tracks"
        );
        q.clear();
        assert_eq!(q.loop_set(), None);
    }

    #[test]
    fn empty_queue_is_inert() {
        let mut q = Queue::new();
        assert_eq!(q.next(), None);
        assert_eq!(q.previous(), None);
        assert_eq!(q.advance_on_eof(), None);
        assert!(q.is_empty());
    }
}
