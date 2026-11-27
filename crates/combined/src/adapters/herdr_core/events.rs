//! `events.subscribe` lines mapped to [`TermEvent`], and the diff that covers what a lost
//! subscription missed.
//!
//! Event names come from `hc schema`. The field names come from the committed capture
//! (`fixtures/`, written by `sh tools/verify-herdr-core --capture`): every event is a JSON line
//! `{"method":"event","params":{"type","terminal","seq","data"}}`. Unknown kinds, unknown fields
//! and unparsable lines are ignored. Terminals of another machine never leave this module.

use std::collections::BTreeMap;

use serde_json::Value;

use super::jsonrpc::{is_remote, term_id, WireTerminal};
use crate::model::{TermEvent, TermInfo, TermState};

/// What one line of the subscription is.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Line {
    Event {
        seq: u64,
        address: String,
        kind: String,
        data: Value,
    },
    /// The subscriber fell behind; a fresh snapshot follows on a new subscription.
    Missed,
    Heartbeat,
    Other,
}

pub(super) fn parse_line(line: &str) -> Line {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Line::Other;
    };
    let method = value.get("method").and_then(Value::as_str);
    let params = value.get("params");
    let kind = params
        .and_then(|params| params.get("type"))
        .and_then(Value::as_str)
        .or_else(|| value.get("type").and_then(Value::as_str));
    for name in [method, kind].into_iter().flatten() {
        match name {
            "missed" => return Line::Missed,
            "heartbeat" => return Line::Heartbeat,
            _ => {}
        }
    }
    let (Some(params), Some(kind)) = (params, kind) else {
        return Line::Other;
    };
    let (Some(seq), Some(address)) = (
        params.get("seq").and_then(Value::as_u64),
        params.get("terminal").and_then(Value::as_str),
    ) else {
        return Line::Other;
    };
    Line::Event {
        seq,
        address: address.to_string(),
        kind: kind.to_string(),
        data: params.get("data").cloned().unwrap_or(Value::Null),
    }
}

/// What a subscription's snapshot says.
#[derive(Debug, Clone)]
pub(super) struct Snapshot {
    pub seq: u64,
    pub channel: Option<u32>,
    pub terminals: BTreeMap<String, TermInfo>,
}

/// The local terminals of a `SubscribeResult`.
pub(super) fn parse_snapshot(result: &Value) -> Option<Snapshot> {
    let seq = result.get("seq")?.as_u64()?;
    let channel = result
        .get("channel")
        .and_then(Value::as_u64)
        .and_then(|channel| u32::try_from(channel).ok());
    let wire: Vec<WireTerminal> = serde_json::from_value(result.get("terminals")?.clone()).ok()?;
    let terminals = wire
        .iter()
        .filter(|terminal| !is_remote(&terminal.address))
        .map(|terminal| (terminal.address.clone(), terminal.to_info()))
        .collect();
    Some(Snapshot {
        seq,
        channel,
        terminals,
    })
}

/// Why a fresh snapshot is being compared with the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DiffMode {
    /// The subscriber fell behind; the core is the same.
    Missed,
    /// The connection ended; the core may have restarted, so every survivor reports it.
    Reconnect,
}

/// What a watcher knows: the last sequence number and the terminals as they were.
#[derive(Debug, Default)]
pub(super) struct WatchState {
    pub last_seq: u64,
    pub cache: BTreeMap<String, TermInfo>,
}

/// The result of one line.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Applied {
    pub events: Vec<TermEvent>,
    pub missed: bool,
}

impl WatchState {
    /// Replaces the cache with a snapshot and reports nothing.
    pub(super) fn seed(&mut self, snapshot: &Snapshot) {
        self.last_seq = snapshot.seq;
        self.cache = snapshot.terminals.clone();
    }

    /// Adopts a fresh snapshot and returns what changed since the cache.
    pub(super) fn reseed(&mut self, snapshot: &Snapshot, mode: DiffMode) -> Vec<TermEvent> {
        let events = diff(&self.cache, &snapshot.terminals, mode);
        self.seed(snapshot);
        events
    }

    /// Reports every cached terminal as lost, exited ones included, and forgets them: once the
    /// cache has forgotten an exited terminal, no later diff could take it out of the list.
    pub(super) fn lose_all(&mut self) -> Vec<TermEvent> {
        let events = self
            .cache
            .values()
            .map(|info| TermEvent::Lost {
                id: info.id.clone(),
            })
            .collect();
        self.cache.clear();
        events
    }

    pub(super) fn apply_line(&mut self, line: &str) -> Applied {
        match parse_line(line) {
            Line::Missed => Applied {
                events: Vec::new(),
                missed: true,
            },
            Line::Heartbeat | Line::Other => Applied::default(),
            Line::Event {
                seq,
                address,
                kind,
                data,
            } => {
                if seq <= self.last_seq || is_remote(&address) {
                    return Applied::default();
                }
                self.last_seq = seq;
                Applied {
                    events: self.map_event(&address, &kind, &data),
                    missed: false,
                }
            }
        }
    }

    fn map_event(&mut self, address: &str, kind: &str, data: &Value) -> Vec<TermEvent> {
        let id = term_id(address);
        match kind {
            "created" => {
                let Ok(wire) = serde_json::from_value::<WireTerminal>(data.clone()) else {
                    return Vec::new();
                };
                if is_remote(&wire.address) {
                    return Vec::new();
                }
                let info = wire.to_info();
                self.cache.insert(address.to_string(), info.clone());
                vec![TermEvent::Created(info)]
            }
            "finished" => {
                let exit_code = data
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .and_then(|code| i32::try_from(code).ok());
                if let Some(info) = self.cache.get_mut(address) {
                    info.state = TermState::Exited { exit_code };
                }
                vec![TermEvent::Exited { id, exit_code }]
            }
            "lost" => {
                self.cache.remove(address);
                vec![TermEvent::Lost { id }]
            }
            "restarted" => vec![TermEvent::Restarted { id }],
            // The core no longer has the terminal, running or exited: an exited one leaves the
            // list too, so a pinned tab reads as gone rather than exited forever.
            "removed" => match self.cache.remove(address) {
                Some(_) => vec![TermEvent::Lost { id }],
                None => Vec::new(),
            },
            "labels" => self.change(address, |info| {
                let labels = data.get("labels").unwrap_or(data);
                if let Some(map) = labels.as_object() {
                    info.labels = map
                        .iter()
                        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                        .collect();
                }
            }),
            "size" => self.change(address, |info| {
                let dimension = |name: &str| {
                    data.get(name)
                        .and_then(Value::as_u64)
                        .and_then(|value| u16::try_from(value).ok())
                };
                if let (Some(cols), Some(rows)) = (dimension("cols"), dimension("rows")) {
                    info.cols = cols;
                    info.rows = rows;
                }
            }),
            _ => Vec::new(),
        }
    }

    fn change(&mut self, address: &str, update: impl FnOnce(&mut TermInfo)) -> Vec<TermEvent> {
        match self.cache.get_mut(address) {
            Some(info) => {
                update(info);
                vec![TermEvent::Changed(info.clone())]
            }
            None => Vec::new(),
        }
    }
}

/// What happened between two snapshots, in address order.
pub(super) fn diff(
    before: &BTreeMap<String, TermInfo>,
    after: &BTreeMap<String, TermInfo>,
    mode: DiffMode,
) -> Vec<TermEvent> {
    let mut events = Vec::new();
    for (address, old) in before {
        match after.get(address) {
            // Missing from the fresh snapshot: the core removed it, whether it ran or had exited.
            None => events.push(TermEvent::Lost { id: old.id.clone() }),
            Some(new) => match (&old.state, &new.state) {
                (TermState::Running, TermState::Exited { exit_code }) => {
                    events.push(TermEvent::Exited {
                        id: new.id.clone(),
                        exit_code: *exit_code,
                    });
                }
                (TermState::Running, TermState::Running) => {
                    if old != new {
                        events.push(TermEvent::Changed(new.clone()));
                    }
                    if mode == DiffMode::Reconnect {
                        events.push(TermEvent::Restarted { id: new.id.clone() });
                    }
                }
                _ => {}
            },
        }
    }
    for (address, new) in after {
        if !before.contains_key(address) && new.state == TermState::Running {
            events.push(TermEvent::Created(new.clone()));
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use combine_protocol::{BackendInfo, TermId, TerminalState};

    use super::*;
    use crate::core::TerminalList;
    use crate::ports::{GitEntry, GitProbe};

    fn core_a() -> BackendInfo {
        BackendInfo {
            id: "herdr-core".to_string(),
            instance: Some("core-a".to_string()),
        }
    }

    const EVENTS: &str = include_str!("fixtures/events.jsonl");
    const SUBSCRIBE_1: &str = include_str!("fixtures/subscribe-1.json");
    const SUBSCRIBE_2: &str = include_str!("fixtures/subscribe-2.json");

    fn snapshot(text: &str) -> Snapshot {
        parse_snapshot(&serde_json::from_str(text).expect("json")).expect("a snapshot")
    }

    fn seq_of(line: &str) -> Option<u64> {
        let value: Value = serde_json::from_str(line).ok()?;
        value.pointer("/params/seq")?.as_u64()
    }

    /// The captured lines, cut where the sequence number restarts: the core restarted.
    fn subscriptions() -> Vec<Vec<&'static str>> {
        let mut all: Vec<Vec<&'static str>> = vec![Vec::new()];
        let mut last = 0;
        for line in EVENTS.lines() {
            let seq = seq_of(line).unwrap_or(last);
            if seq < last {
                all.push(Vec::new());
            }
            last = seq;
            all.last_mut().expect("a group").push(line);
        }
        all
    }

    fn first_size_line() -> &'static str {
        EVENTS
            .lines()
            .find(|line| line.contains("\"type\":\"size\""))
            .expect("a captured size event")
    }

    fn running(address: &str) -> TermInfo {
        TermInfo {
            id: term_id(address),
            cwd: "/tmp".to_string(),
            argv: Vec::new(),
            state: TermState::Running,
            labels: BTreeMap::new(),
            cols: 80,
            rows: 24,
        }
    }

    fn state_of(snapshot_text: &str) -> WatchState {
        let mut state = WatchState::default();
        state.seed(&snapshot(snapshot_text));
        state
    }

    #[test]
    fn every_captured_line_parses_or_is_ignored() {
        assert!(EVENTS.lines().count() > 8);
        let mut state = state_of(SUBSCRIBE_1);
        for line in EVENTS.lines() {
            let parsed = parse_line(line);
            assert_ne!(parsed, Line::Other, "{line}");
            if let Line::Event { kind, .. } = &parsed {
                assert!(
                    [
                        "created",
                        "labels",
                        "size",
                        "finished",
                        "removed",
                        "foreground",
                        "keep",
                        "status",
                        "turn",
                        "title",
                        "doc",
                        "lost",
                        "restarted"
                    ]
                    .contains(&kind.as_str()),
                    "an event kind the capture shows but the adapter hasn't looked at: {kind}"
                );
            }
            state.apply_line(line);
        }
    }

    #[test]
    fn captured_scenario_maps_to_term_events() {
        let subscriptions = subscriptions();
        let mut state = state_of(SUBSCRIBE_1);
        let events: Vec<TermEvent> = subscriptions[0]
            .iter()
            .flat_map(|line| state.apply_line(line).events)
            .collect();
        let [created, labelled, grown, restored, exited, removed, created_b] = events.as_slice()
        else {
            panic!("expected seven events, got {events:?}");
        };
        assert!(matches!(created, TermEvent::Created(info)
            if info.id == term_id("t2") && info.state == TermState::Running));
        assert!(matches!(labelled, TermEvent::Changed(info)
            if info.labels.get("captured").map(String::as_str) == Some("yes")));
        assert!(matches!(grown, TermEvent::Changed(info) if (info.cols, info.rows) == (100, 30)));
        assert!(
            matches!(restored, TermEvent::Changed(info) if (info.cols, info.rows) == (200, 120))
        );
        assert!(matches!(exited, TermEvent::Exited { id, .. } if *id == term_id("t2")));
        assert_eq!(
            *removed,
            TermEvent::Lost { id: term_id("t2") },
            "an exited terminal the core removed leaves the list"
        );
        assert!(matches!(created_b, TermEvent::Created(info) if info.id == term_id("t3")));
        assert!(
            !state.cache.contains_key("t2"),
            "a removed terminal leaves the cache"
        );
    }

    #[test]
    fn snapshot_seeds_without_events() {
        let state = state_of(SUBSCRIBE_1);
        let snapshot = snapshot(SUBSCRIBE_1);
        assert_eq!(state.last_seq, snapshot.seq);
        assert_eq!(state.cache.keys().collect::<Vec<_>>(), vec!["t1"]);
        assert_eq!(snapshot.channel, Some(1));
    }

    #[test]
    fn events_at_or_below_the_snapshot_seq_are_ignored() {
        let mut state = state_of(SUBSCRIBE_1);
        let created = subscriptions()[0][0];
        assert_eq!(state.apply_line(created).events.len(), 1);
        assert_eq!(
            state.apply_line(created),
            Applied::default(),
            "the same line again"
        );
        let mut ahead = state_of(SUBSCRIBE_1);
        ahead.last_seq = 1_000;
        assert_eq!(ahead.apply_line(created), Applied::default());
    }

    #[test]
    fn captured_heartbeat_is_ignored() {
        let heartbeat = EVENTS
            .lines()
            .find(|line| line.contains("heartbeat"))
            .expect("a captured heartbeat");
        assert_eq!(parse_line(heartbeat), Line::Heartbeat);
        let mut state = state_of(SUBSCRIBE_1);
        let before = state.last_seq;
        assert_eq!(state.apply_line(heartbeat), Applied::default());
        assert_eq!(state.last_seq, before);
    }

    #[test]
    fn missed_resubscribes_and_diffs_the_fresh_snapshot() {
        let missed = first_size_line().replace("\"type\":\"size\"", "\"type\":\"missed\"");
        let mut state = state_of(SUBSCRIBE_1);
        let applied = state.apply_line(&missed);
        assert!(applied.missed && applied.events.is_empty());
        let fresh = snapshot(SUBSCRIBE_2);
        let events = state.reseed(&fresh, DiffMode::Missed);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], TermEvent::Created(info) if info.id == term_id("t3")));
        assert_eq!(state.last_seq, fresh.seq);
        assert_eq!(state.cache.len(), 2);
    }

    /// The captured `created` → `finished` → `removed` of one terminal, through the daemon's
    /// terminal list: it is listed running, then exited (its tab keeps its screen), then not at
    /// all (its tab reads as gone).
    #[test]
    fn a_finished_then_removed_terminal_leaves_the_daemon_snapshot() {
        let mut state = state_of(SUBSCRIBE_1);
        let mut list = TerminalList::new(Box::new(NoGit), core_a());
        list.apply_list(&state.cache.values().cloned().collect::<Vec<_>>());
        let t2 = term_id("t2");
        let state_of_t2 = |list: &TerminalList| {
            list.snapshot(None)
                .terminals
                .into_iter()
                .find(|terminal| terminal.id == t2)
                .map(|terminal| terminal.state)
        };
        let mut seen = Vec::new();
        for line in &subscriptions()[0] {
            let kind = match parse_line(line) {
                Line::Event { kind, .. } => kind,
                _ => continue,
            };
            for event in state.apply_line(line).events {
                list.apply_event(&event);
            }
            if ["created", "finished", "removed"].contains(&kind.as_str())
                && line.contains("\"t2\"")
            {
                seen.push((kind, state_of_t2(&list)));
            }
        }
        assert_eq!(
            seen,
            vec![
                ("created".to_string(), Some(TerminalState::Running)),
                ("finished".to_string(), Some(TerminalState::Exited)),
                ("removed".to_string(), None),
            ]
        );
        assert!(list
            .snapshot(None)
            .spaces
            .iter()
            .all(|space| !space.terminals.contains(&t2)));
    }

    #[test]
    fn an_exited_terminal_missing_from_a_replacement_snapshot_is_removed() {
        let mut exited = running("t2");
        exited.state = TermState::Exited { exit_code: Some(0) };
        let before: BTreeMap<String, TermInfo> = [
            ("t1".to_string(), running("t1")),
            ("t2".to_string(), exited.clone()),
            ("t5".to_string(), {
                let mut kept = running("t5");
                kept.state = TermState::Exited { exit_code: None };
                kept
            }),
        ]
        .into_iter()
        .collect();
        let mut still = running("t5");
        still.state = TermState::Exited { exit_code: None };
        let after: BTreeMap<String, TermInfo> =
            [("t1".to_string(), running("t1")), ("t5".to_string(), still)]
                .into_iter()
                .collect();
        for mode in [DiffMode::Missed, DiffMode::Reconnect] {
            let events = diff(&before, &after, mode);
            assert!(
                events.contains(&TermEvent::Lost { id: term_id("t2") }),
                "{mode:?}: {events:?}"
            );
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, TermEvent::Lost { id } if *id != term_id("t2"))),
                "{mode:?}: only the missing terminal is removed: {events:?}"
            );
        }

        let mut state = WatchState {
            last_seq: 0,
            cache: before,
        };
        let mut list = TerminalList::new(Box::new(NoGit), core_a());
        list.apply_list(&state.cache.values().cloned().collect::<Vec<_>>());
        for event in state.reseed(
            &Snapshot {
                seq: 0,
                channel: None,
                terminals: after,
            },
            DiffMode::Reconnect,
        ) {
            list.apply_event(&event);
        }
        let ids: Vec<TermId> = list
            .snapshot(None)
            .terminals
            .into_iter()
            .map(|terminal| terminal.id)
            .collect();
        assert_eq!(ids, vec![term_id("t1"), term_id("t5")]);
    }

    #[test]
    fn losing_the_core_removes_exited_terminals_too() {
        let mut exited = running("t2");
        exited.state = TermState::Exited { exit_code: Some(1) };
        let mut state = WatchState {
            last_seq: 0,
            cache: [
                ("t1".to_string(), running("t1")),
                ("t2".to_string(), exited),
            ]
            .into_iter()
            .collect(),
        };
        assert_eq!(
            state.lose_all(),
            vec![
                TermEvent::Lost { id: term_id("t1") },
                TermEvent::Lost { id: term_id("t2") },
            ]
        );
        assert!(state.cache.is_empty());
    }

    struct NoGit;

    impl GitProbe for NoGit {
        fn git_entry(&self, _dir: &str) -> GitEntry {
            GitEntry::Absent
        }
    }

    #[test]
    fn reconnect_diff_reports_restarted_lost_created_exited() {
        let before: BTreeMap<String, TermInfo> = ["t1", "t2", "t4"]
            .iter()
            .map(|address| (address.to_string(), running(address)))
            .collect();
        let mut gone = running("t4");
        gone.state = TermState::Exited { exit_code: Some(3) };
        let after: BTreeMap<String, TermInfo> = [
            ("t1".to_string(), running("t1")),
            ("t3".to_string(), running("t3")),
            ("t4".to_string(), gone),
        ]
        .into_iter()
        .collect();
        let events = diff(&before, &after, DiffMode::Reconnect);
        assert_eq!(
            events,
            vec![
                TermEvent::Restarted { id: term_id("t1") },
                TermEvent::Lost { id: term_id("t2") },
                TermEvent::Exited {
                    id: term_id("t4"),
                    exit_code: Some(3)
                },
                TermEvent::Created(running("t3")),
            ]
        );
        let missed = diff(&before, &after, DiffMode::Missed);
        assert!(!missed
            .iter()
            .any(|event| matches!(event, TermEvent::Restarted { .. })));
    }

    #[test]
    fn derived_kinds_map_by_address_and_type() {
        let size = first_size_line();
        let mut state = state_of(SUBSCRIBE_2);
        let line = |kind: &str, seq: u64| {
            size.replace("\"type\":\"size\"", &format!("\"type\":\"{kind}\""))
                .replace(
                    &format!("\"seq\":{}", seq_of(size).expect("seq")),
                    &format!("\"seq\":{seq}"),
                )
                .replace("\"terminal\":\"t3\"", "\"terminal\":\"t1\"")
                .replace("\"terminal\":\"t2\"", "\"terminal\":\"t1\"")
        };
        let restarted = state.apply_line(&line("restarted", 10));
        assert_eq!(
            restarted.events,
            vec![TermEvent::Restarted { id: term_id("t1") }]
        );
        let removed = state.apply_line(&line("removed", 11));
        assert_eq!(removed.events, vec![TermEvent::Lost { id: term_id("t1") }]);
        let again = state.apply_line(&line("removed", 12));
        assert!(
            again.events.is_empty(),
            "removing what isn't there says nothing"
        );
        let lost = state
            .apply_line(&line("lost", 13).replace("\"terminal\":\"t1\"", "\"terminal\":\"t3\""));
        assert_eq!(lost.events, vec![TermEvent::Lost { id: term_id("t3") }]);
        for ignored in ["keep", "status", "turn", "title", "doc"] {
            assert!(
                state.apply_line(&line(ignored, 20)).events.is_empty(),
                "{ignored}"
            );
        }
    }

    #[test]
    fn unknown_kinds_and_fields_are_ignored() {
        let mut state = state_of(SUBSCRIBE_1);
        let unknown = subscriptions()[0][0].replace("\"type\":\"created\"", "\"type\":\"novel\"");
        assert_eq!(state.apply_line(&unknown), Applied::default());
        let extra = subscriptions()[0][0]
            .replace("\"data\":{", "\"future\":1,\"data\":{\"added_later\":[1],");
        assert_eq!(state_of(SUBSCRIBE_1).apply_line(&extra).events.len(), 1);
        for junk in [
            "",
            "not json",
            "{}",
            "[1,2]",
            "{\"params\":{\"type\":\"created\"}}",
        ] {
            assert_eq!(state.apply_line(junk), Applied::default(), "{junk:?}");
        }
    }

    #[test]
    fn remote_snapshot_entries_are_dropped() {
        let rewritten = SUBSCRIBE_1.replace("\"address\":\"t1\"", "\"address\":\"work/t1\"");
        assert_ne!(rewritten, SUBSCRIBE_1);
        assert!(snapshot(&rewritten).terminals.is_empty());
        let both = SUBSCRIBE_2.replacen("\"address\":\"t3\"", "\"address\":\"work/t3\"", 1);
        assert_eq!(
            snapshot(&both).terminals.keys().collect::<Vec<_>>(),
            vec!["t1"]
        );
    }

    #[test]
    fn remote_events_are_dropped() {
        let mut state = state_of(SUBSCRIBE_1);
        for line in subscriptions()[0].iter() {
            let remote = line.replace("\"terminal\":\"t2\"", "\"terminal\":\"work/t2\"");
            let applied = state.apply_line(&remote);
            if remote != *line {
                assert_eq!(applied, Applied::default(), "{remote}");
            }
        }
        let kept: Vec<&String> = state.cache.keys().collect();
        assert_eq!(kept, vec!["t1", "t3"], "t2 never appears; B does");
    }
}
