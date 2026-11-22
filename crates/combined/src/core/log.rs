//! One attachment's event log: the outbound buffer, its cursors and its bounds.
//!
//! The log is what a client reads from. Entries stay in it from the moment they are appended
//! until the client acks them, so a resume can replay them. Every bound in the module follows
//! from one rule: the log never holds more than `max_bytes` and `max_events`, counting the
//! credit it has granted and a reserved slot for the final gap.

use std::collections::VecDeque;

use combine_protocol::{
    AttachmentId, BackendGeneration, Closed, Cursor, DaemonMessage, Data, Epoch, Facts, Fidelity,
    Gap, Lost, Resized, Resynced, Seq, Started, MAX_DATA_CHUNK,
};

use super::config::{data_margin, CoreConfig, EVENT_OVERHEAD};
use crate::ports::AttachEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogLimits {
    pub max_bytes: usize,
    pub max_events: usize,
    pub credit_reserve: usize,
    pub credit_min_grant: u32,
}

impl From<&CoreConfig> for LogLimits {
    fn from(config: &CoreConfig) -> Self {
        LogLimits {
            max_bytes: config.log_max_bytes,
            max_events: config.log_max_events,
            credit_reserve: config.credit_reserve,
            credit_min_grant: config.credit_min_grant,
        }
    }
}

impl LogLimits {
    /// The `(bytes, events)` of headroom kept for the entries that outstanding credit may still
    /// create: with K full-size chunks filling the log, `(64 * (K + 2), K + 2)`.
    pub fn data_margin(&self) -> (usize, usize) {
        data_margin(self.max_bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appended {
    Yes,
    /// The event did not fit. The log has ended with the gap.
    Overflowed,
    /// The log has already ended, or has no epoch to append to.
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckError {
    /// The cursor names an entry that was never sent.
    NotSent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeError {
    /// The log cannot replay from that cursor.
    NotServable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogEnd {
    /// A closed or lost event ended the stream.
    Final,
    /// The log overflowed, or ended before its first epoch, and the stream ends with a gap.
    Gap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogStats {
    pub retained_bytes: usize,
    pub events: usize,
    pub outstanding_credit: u64,
    /// Retained bytes, outstanding credit and the gap reservation.
    pub accounted_bytes: usize,
    pub peak_accounted_bytes: usize,
    pub peak_events: usize,
    /// The open entry is waiting for room before it is sent.
    pub held_back: bool,
    pub ended: Option<LogEnd>,
}

#[derive(Debug)]
struct Entry {
    cursor: Cursor,
    message: DaemonMessage,
    cost: usize,
}

#[derive(Debug)]
pub struct EventLog {
    attachment: AttachmentId,
    limits: LogLimits,
    byte_credit: bool,
    epoch: u64,
    next_seq: u64,
    entries: VecDeque<Entry>,
    /// Index of the first entry not yet sent.
    next_send: usize,
    acked: Option<Cursor>,
    retained_bytes: usize,
    outstanding: u64,
    /// The last entry is `Data` of the current epoch that was never sent and nothing has sealed.
    open: bool,
    ended: Option<LogEnd>,
    gap_sent: bool,
    peak_accounted: usize,
    peak_events: usize,
}

fn entry_cost(message: &DaemonMessage) -> usize {
    match message {
        DaemonMessage::Data(data) => data.bytes.len() + EVENT_OVERHEAD,
        DaemonMessage::Facts(facts) => facts_cost(&facts.display),
        _ => EVENT_OVERHEAD,
    }
}

fn facts_cost(display: &combine_protocol::Display) -> usize {
    EVENT_OVERHEAD
        + display.foreground.len()
        + display.background.len()
        + display.cursor_color.len()
}

fn key(cursor: Cursor) -> (u64, u64) {
    (cursor.epoch.0, cursor.seq.0)
}

impl EventLog {
    pub fn new(attachment: AttachmentId, limits: LogLimits, byte_credit: bool) -> Self {
        EventLog {
            attachment,
            limits,
            byte_credit,
            epoch: 0,
            next_seq: 0,
            entries: VecDeque::new(),
            next_send: 0,
            acked: None,
            retained_bytes: 0,
            outstanding: 0,
            open: false,
            ended: None,
            gap_sent: false,
            peak_accounted: 0,
            peak_events: 0,
        }
    }

    fn cursor_for(&self, seq: u64) -> Cursor {
        Cursor {
            attachment: self.attachment,
            epoch: Epoch(self.epoch),
            seq: Seq(seq),
        }
    }

    fn take_cursor(&mut self) -> Cursor {
        let cursor = self.cursor_for(self.next_seq);
        self.next_seq += 1;
        cursor
    }

    fn accounted(&self) -> usize {
        let outstanding = usize::try_from(self.outstanding).unwrap_or(usize::MAX);
        self.retained_bytes
            .saturating_add(outstanding)
            .saturating_add(EVENT_OVERHEAD)
    }

    fn touch(&mut self) {
        self.peak_accounted = self.peak_accounted.max(self.accounted());
        self.peak_events = self.peak_events.max(self.entries.len());
    }

    /// Free bytes and events if the log held `retained` bytes, `events` entries and `outstanding`
    /// credit. Negative when over the bound.
    fn headroom(&self, retained: usize, events: usize, outstanding: u64) -> (i128, i128) {
        let bytes = self.limits.max_bytes as i128
            - EVENT_OVERHEAD as i128
            - retained as i128
            - outstanding as i128;
        let slots = self.limits.max_events as i128 - 1 - events as i128;
        (bytes, slots)
    }

    fn current_headroom(&self) -> (i128, i128) {
        self.headroom(self.retained_bytes, self.entries.len(), self.outstanding)
    }

    fn fits_margin(&self, retained: usize, events: usize, outstanding: u64) -> bool {
        let (margin_bytes, margin_events) = self.limits.data_margin();
        let (bytes, slots) = self.headroom(retained, events, outstanding);
        bytes >= margin_bytes as i128 && slots >= margin_events as i128
    }

    fn seal(&mut self) {
        self.open = false;
    }

    fn push(&mut self, cursor: Cursor, message: DaemonMessage) {
        let cost = entry_cost(&message);
        self.retained_bytes += cost;
        self.entries.push_back(Entry {
            cursor,
            message,
            cost,
        });
    }

    /// Ends the log with the gap: the stream runs to the last retained entry, then the gap.
    fn overflow(&mut self) -> Appended {
        self.end_with_gap();
        Appended::Overflowed
    }

    /// Opens the next epoch with a `Started` at seq 0 and resets the credit allowance to
    /// `initial_credit` (0 without byte credit).
    ///
    /// The `Started` must leave the data margin free beside the new allowance, because the
    /// old pair's credit is void.
    pub fn start_epoch(
        &mut self,
        generation: BackendGeneration,
        cols: u16,
        rows: u16,
        fidelity: Fidelity,
        ordered_resizes: bool,
        initial_credit: u32,
    ) -> Appended {
        if self.ended.is_some() {
            return Appended::Ignored;
        }
        let allowance = if self.byte_credit {
            u64::from(initial_credit)
        } else {
            0
        };
        let retained = self.retained_bytes + EVENT_OVERHEAD;
        if !self.fits_margin(retained, self.entries.len() + 1, allowance) {
            return self.overflow();
        }
        self.seal();
        self.epoch += 1;
        self.next_seq = 0;
        let cursor = self.take_cursor();
        self.push(
            cursor,
            DaemonMessage::Started(Started {
                cursor,
                backend_generation: generation,
                cols,
                rows,
                fidelity,
                ordered_resizes,
            }),
        );
        self.outstanding = allowance;
        self.touch();
        Appended::Yes
    }

    /// Appends `Data`, `Resized`, `Facts` or `Resynced`. `Closed` and `Lost` end the log with a
    /// final event. `Started` is ignored: use [`start_epoch`](Self::start_epoch).
    pub fn append(&mut self, event: &AttachEvent) -> Appended {
        if self.ended.is_some() || self.epoch == 0 {
            return Appended::Ignored;
        }
        match event {
            AttachEvent::Started { .. } => Appended::Ignored,
            AttachEvent::Data(bytes) => self.append_data(bytes),
            AttachEvent::Resized { cols, rows } => {
                let cursor = self.cursor_for(self.next_seq);
                let message = DaemonMessage::Resized(Resized {
                    cursor,
                    cols: *cols,
                    rows: *rows,
                });
                self.append_margin(cursor, message)
            }
            AttachEvent::Facts(display) => {
                let cursor = self.cursor_for(self.next_seq);
                let message = DaemonMessage::Facts(Facts {
                    cursor,
                    display: display.clone(),
                });
                self.append_margin(cursor, message)
            }
            AttachEvent::Resynced => {
                let cursor = self.cursor_for(self.next_seq);
                let message = DaemonMessage::Resynced(Resynced { cursor });
                self.append_margin(cursor, message)
            }
            AttachEvent::Closed(reason) => {
                let cursor = self.cursor_for(self.next_seq);
                let message = DaemonMessage::Closed(Closed {
                    cursor,
                    reason: *reason,
                });
                self.append_final(cursor, message)
            }
            AttachEvent::Lost => {
                let cursor = self.cursor_for(self.next_seq);
                let message = DaemonMessage::Lost(Lost { cursor });
                self.append_final(cursor, message)
            }
        }
    }

    fn append_data(&mut self, bytes: &[u8]) -> Appended {
        if bytes.is_empty() {
            return Appended::Yes;
        }
        let room_in_open = match (self.open, self.entries.back()) {
            (
                true,
                Some(Entry {
                    message: DaemonMessage::Data(data),
                    ..
                }),
            ) => MAX_DATA_CHUNK.saturating_sub(data.bytes.len()),
            _ => 0,
        };
        let merged = bytes.len().min(room_in_open);
        let rest = bytes.len() - merged;
        let new_entries = rest.div_ceil(MAX_DATA_CHUNK);
        let retained = self.retained_bytes + bytes.len() + EVENT_OVERHEAD * new_entries;
        let events = self.entries.len() + new_entries;
        let outstanding = self.outstanding.saturating_sub(bytes.len() as u64);
        let (free_bytes, free_events) = self.headroom(retained, events, outstanding);
        if free_bytes < 0 || free_events < 0 {
            return self.overflow();
        }
        let (head, tail) = bytes.split_at(merged);
        if merged > 0 {
            if let Some(Entry {
                message: DaemonMessage::Data(data),
                cost,
                ..
            }) = self.entries.back_mut()
            {
                data.bytes.extend_from_slice(head);
                *cost += merged;
            }
            self.retained_bytes += merged;
        }
        for chunk in tail.chunks(MAX_DATA_CHUNK) {
            let cursor = self.take_cursor();
            self.push(
                cursor,
                DaemonMessage::Data(Data {
                    cursor,
                    bytes: chunk.to_vec(),
                }),
            );
            self.open = true;
        }
        self.outstanding = outstanding;
        self.touch();
        Appended::Yes
    }

    /// Appends an entry that must leave the data margin free.
    fn append_margin(&mut self, cursor: Cursor, message: DaemonMessage) -> Appended {
        let cost = entry_cost(&message);
        if !self.fits_margin(
            self.retained_bytes + cost,
            self.entries.len() + 1,
            self.outstanding,
        ) {
            return self.overflow();
        }
        self.seal();
        self.next_seq += 1;
        self.push(cursor, message);
        self.touch();
        Appended::Yes
    }

    fn append_final(&mut self, cursor: Cursor, message: DaemonMessage) -> Appended {
        let cost = entry_cost(&message);
        let (free_bytes, free_events) = self.headroom(
            self.retained_bytes + cost,
            self.entries.len() + 1,
            self.outstanding,
        );
        if free_bytes < 0 || free_events < 0 {
            return self.overflow();
        }
        self.seal();
        self.next_seq += 1;
        self.push(cursor, message);
        self.ended = Some(LogEnd::Final);
        self.outstanding = 0;
        self.touch();
        Appended::Yes
    }

    /// Ends the log with the gap. Nothing is dropped or reordered: the stream still runs to the
    /// last retained entry first. Does nothing if the log has already ended.
    pub fn end_with_gap(&mut self) {
        if self.ended.is_some() {
            return;
        }
        self.seal();
        self.outstanding = 0;
        self.ended = Some(LogEnd::Gap);
        self.touch();
    }

    fn trim_through(&mut self, index: usize) {
        for _ in 0..=index {
            if let Some(entry) = self.entries.pop_front() {
                self.retained_bytes -= entry.cost;
            }
        }
        self.next_send -= index + 1;
    }

    /// Trims the entries through the one at `cursor`, which the client has applied.
    ///
    /// `Ok(true)` means the final event is now acked. A late or duplicate ack is `Ok(false)`
    /// and changes nothing.
    pub fn ack(&mut self, cursor: Cursor) -> Result<bool, AckError> {
        if cursor.attachment != self.attachment {
            return Err(AckError::NotSent);
        }
        if self.acked == Some(cursor) {
            return Ok(false);
        }
        if let Some(acked) = self.acked {
            if key(cursor) <= key(acked) {
                return Ok(false);
            }
        }
        if let Some(first) = self.entries.front() {
            if key(cursor) < key(first.cursor) {
                return Ok(false);
            }
        }
        let Some(index) = self.entries.iter().position(|e| e.cursor == cursor) else {
            return Err(AckError::NotSent);
        };
        if index >= self.next_send {
            return Err(AckError::NotSent);
        }
        let final_event = matches!(
            self.entries[index].message,
            DaemonMessage::Closed(_) | DaemonMessage::Lost(_)
        );
        self.trim_through(index);
        self.acked = Some(cursor);
        Ok(final_event)
    }

    /// Rewinds the log to replay every entry after `cursor`, sent or not.
    ///
    /// Returns the cursor of the last entry that will be replayed, or `None` when there is
    /// nothing to replay. A log that ended with the gap can't be resumed. While the log is live,
    /// one without room for its data margin can't either; an ended log appends nothing more, so
    /// it skips that check.
    pub fn resume(&mut self, cursor: Cursor) -> Result<Option<Cursor>, ResumeError> {
        if self.ended == Some(LogEnd::Gap) {
            return Err(ResumeError::NotServable);
        }
        if self.acked != Some(cursor) {
            let index = self
                .entries
                .iter()
                .position(|e| e.cursor == cursor)
                .filter(|index| *index < self.next_send)
                .ok_or(ResumeError::NotServable)?;
            self.trim_through(index);
            self.acked = Some(cursor);
        }
        if self.ended.is_none() {
            let (margin_bytes, margin_events) = self.limits.data_margin();
            let (bytes, slots) = self.current_headroom();
            if bytes < margin_bytes as i128 || slots < margin_events as i128 {
                return Err(ResumeError::NotServable);
            }
        }
        self.seal();
        self.next_send = 0;
        Ok(self.entries.back().map(|entry| entry.cursor))
    }

    /// Whether the open entry may be sent: always once the log has ended, and while live only
    /// with room to spare.
    fn open_entry_may_go(&self) -> bool {
        if self.ended.is_some() {
            return true;
        }
        let (bytes, slots) = self.current_headroom();
        bytes >= (self.limits.credit_reserve / 2) as i128
            && slots >= (self.limits.max_events / 2) as i128
    }

    fn next_sendable_index(&self) -> Option<usize> {
        if self.next_send >= self.entries.len() {
            return None;
        }
        let last = self.entries.len() - 1;
        if self.open && self.next_send == last && !self.open_entry_may_go() {
            return None;
        }
        Some(self.next_send)
    }

    fn gap_due(&self) -> bool {
        self.ended == Some(LogEnd::Gap) && !self.gap_sent && self.next_send >= self.entries.len()
    }

    /// The next sendable entry, or the final gap once, after the last entry.
    pub fn next_message(&mut self) -> Option<DaemonMessage> {
        if let Some(index) = self.next_sendable_index() {
            if self.open && index == self.entries.len() - 1 {
                self.seal();
            }
            self.next_send += 1;
            return Some(self.entries[index].message.clone());
        }
        if self.gap_due() {
            self.gap_sent = true;
            return Some(DaemonMessage::Gap(Gap {
                attachment: self.attachment,
                request: None,
            }));
        }
        None
    }

    /// The cost of what [`next_message`](Self::next_message) would return.
    pub fn next_cost(&self) -> Option<usize> {
        if let Some(index) = self.next_sendable_index() {
            return Some(self.entries[index].cost);
        }
        self.gap_due().then_some(EVENT_OVERHEAD)
    }

    pub fn has_sendable(&self) -> bool {
        self.next_cost().is_some()
    }

    /// Grants credit when the log has the room, and records it as outstanding.
    ///
    /// Only with byte credit, and never once the log has ended.
    pub fn take_credit(&mut self) -> Option<u32> {
        if !self.byte_credit || self.ended.is_some() {
            return None;
        }
        let room = self.limits.max_bytes as i128
            - EVENT_OVERHEAD as i128
            - self.limits.credit_reserve as i128
            - self.retained_bytes as i128
            - self.outstanding as i128;
        if room < i128::from(self.limits.credit_min_grant) || room <= 0 {
            return None;
        }
        let grant = u32::try_from(room).unwrap_or(u32::MAX);
        self.outstanding += u64::from(grant);
        self.touch();
        Some(grant)
    }

    pub fn current_epoch(&self) -> Option<Epoch> {
        (self.epoch > 0).then_some(Epoch(self.epoch))
    }

    pub fn ended(&self) -> Option<LogEnd> {
        self.ended
    }

    pub fn gap_sent(&self) -> bool {
        self.gap_sent
    }

    pub fn stats(&self) -> LogStats {
        let held_back =
            self.open && self.next_send + 1 == self.entries.len() && !self.open_entry_may_go();
        LogStats {
            retained_bytes: self.retained_bytes,
            events: self.entries.len(),
            outstanding_credit: self.outstanding,
            accounted_bytes: self.accounted(),
            peak_accounted_bytes: self.peak_accounted,
            peak_events: self.peak_events,
            held_back,
            ended: self.ended,
        }
    }
}
