//! Cases 1-6: the life of a terminal and an attachment, from spawn to its final event.

use combine_protocol::{ClientMessage, CloseReason, Epoch, ErrorCode, Fidelity, Resume, Seq};
use combined::core::CoreConfig;
use combined::model::TermState;
use combined::testing::Half;
use futures::FutureExt;

use crate::common::{display, request_options};
use crate::contract::client::{Applied, Ending, View};
use crate::contract::{Case, ClientSim, Failure, Fault, Harness, Outcome, Target};

fn view_of(c: &ClientSim, attachment: combine_protocol::AttachmentId) -> Result<&View, Failure> {
    c.view(attachment)
        .ok_or_else(|| Failure::new("harness", "the client has no view of the attachment"))
}

pub fn cases<T: Target>() -> Vec<Case<T>> {
    vec![
        Case {
            name: "spawn_attach_write_read_back_resize_kill",
            needs: &[Fault::Observation],
            heavy: false,
            run: |t| spawn_attach_write_read_back_resize_kill(t).boxed_local(),
        },
        Case {
            name: "term_ids_are_prefixed_and_opaque",
            needs: &[],
            heavy: false,
            run: |t| term_ids_are_prefixed_and_opaque(t).boxed_local(),
        },
        Case {
            name: "display_facts_follow_capabilities",
            needs: &[],
            heavy: false,
            run: |t| display_facts_follow_capabilities(t).boxed_local(),
        },
        Case {
            name: "detach_ends_with_a_final_closed",
            needs: &[],
            heavy: false,
            run: |t| detach_ends_with_a_final_closed(t).boxed_local(),
        },
        Case {
            name: "program_exit_is_final",
            needs: &[Fault::BackendRestart],
            heavy: false,
            run: |t| program_exit_is_final(t).boxed_local(),
        },
        Case {
            name: "adapter_reports_lost_only_after_giving_up",
            needs: &[Fault::HalfFailure, Fault::OpenFailure],
            heavy: false,
            run: |t| adapter_reports_lost_only_after_giving_up(t).boxed_local(),
        },
    ]
}

/// Case 1.
pub async fn spawn_attach_write_read_back_resize_kill<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let cwd = h.target.scratch_dir("p");
    let argv = h.target.spawn_spec(&cwd, 80, 24).argv;
    let backend = h.driver.backend().clone();
    let listed = backend
        .list()
        .await
        .map_err(|error| crate::contract::Failure::new("list", format!("list failed: {error}")))?;
    let info = listed.iter().find(|info| info.id == term);
    let Some(info) = info else {
        return Err(Failure::new(
            "list",
            format!("list() is missing {}", term.0),
        ));
    };
    ensure!(
        "list",
        info.cwd == cwd && info.argv == argv,
        "list() reports cwd {:?} argv {:?}, expected {cwd:?} {argv:?}",
        info.cwd,
        info.argv
    );

    let mut c = h.client();
    let att = h
        .attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    let view = view_of(&c, att)?;
    let first = view.started.first();
    ensure!(
        "started",
        first.is_some_and(|started| started.cursor.seq == Seq(0)
            && started.cursor.epoch == Epoch(1)
            && (started.cols, started.rows) == (80, 24)),
        "the first event should be Started at seq 0, epoch 1, 80x24; got {first:?}"
    );

    h.mark(&term, &mut [&mut c]).await?;
    h.type_input(&mut c, att, b"hello").await?;
    h.settle(&mut [&mut c]).await?;
    let view = view_of(&c, att)?;
    ensure!(
        "ownership",
        view.owns(),
        "typing should grant ownership; the client heard {:?}",
        view.ownership
    );
    ensure!(
        "echo",
        view.bytes() == b"hello",
        "expected the typed hello back, got {:?}",
        String::from_utf8_lossy(&view.bytes())
    );

    h.resize(&mut c, att, 100, 30).await?;
    h.settle(&mut [&mut c]).await?;
    let view = view_of(&c, att)?;
    ensure!(
        "resized",
        view.resizes() == [(100, 30)] && view.size == (100, 30),
        "expected one Resized(100,30); the client has {:?}",
        view.resizes()
    );
    let size = h.target.pty_size(&term)?;
    ensure!(
        "pty-size",
        size == (100, 30),
        "the PTY is {size:?}, expected (100, 30)"
    );

    backend
        .kill(&term)
        .await
        .map_err(|error| crate::contract::Failure::new("kill", format!("kill failed: {error}")))?;
    h.settle(&mut [&mut c]).await?;
    let view = view_of(&c, att)?;
    ensure!(
        "closed",
        view.ending == Some(Ending::Closed(CloseReason::Exited))
            && view.transcript.last() == Some(&Applied::Closed(CloseReason::Exited)),
        "the stream should end with Closed(Exited); it ended with {:?}",
        view.ending
    );
    let listed = backend
        .list()
        .await
        .map_err(|error| crate::contract::Failure::new("list", format!("list failed: {error}")))?;
    ensure!(
        "list",
        listed
            .iter()
            .all(|info| info.id != term || info.state != TermState::Running),
        "list() still shows the killed terminal as running"
    );

    let answer = h
        .try_attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    ensure!(
        "unknown-terminal",
        answer
            .as_ref()
            .is_err_and(|error| error.code == ErrorCode::UnknownTerminal),
        "attaching a killed terminal should be unknown_terminal; got {answer:?}"
    );
    h.finish(&[&c])
}

/// Case 2.
pub async fn term_ids_are_prefixed_and_opaque<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let mut prefixes = Vec::new();
    for name in ["a", "b", "c"] {
        let term = h.spawn(name, 80, 24).await?;
        let parts = term.0.split_once(':');
        ensure!(
            "term-id",
            parts.is_some_and(|(prefix, rest)| !prefix.is_empty() && !rest.is_empty()),
            "{:?} is not <prefix>:<address>",
            term.0
        );
        prefixes.push(term.0.split_once(':').map(|(prefix, _)| prefix.to_string()));
    }
    ensure!(
        "term-id",
        prefixes.windows(2).all(|pair| pair[0] == pair[1]),
        "the prefix differs between terminals: {prefixes:?}"
    );
    h.finish(&[])
}

/// Case 3.
pub async fn display_facts_follow_capabilities<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let queries = h.driver.backend().capabilities().display_queries;
    let mut c = h.client();
    let att = h
        .attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    h.request_ownership(&mut c, att).await?;
    h.settle(&mut [&mut c]).await?;
    h.mark(&term, &mut [&mut c]).await?;

    let mut changed = display();
    changed.foreground = "#00ff00".to_string();
    h.set_display(&mut c, att, changed.clone()).await?;
    h.settle(&mut [&mut c]).await?;
    let facts: Vec<&Applied> = view_of(&c, att)?
        .transcript
        .iter()
        .filter(|item| matches!(item, Applied::Facts(_)))
        .collect();
    if queries {
        ensure!(
            "facts",
            facts == [&Applied::Facts(changed)],
            "expected exactly the new Facts, got {facts:?}"
        );
    } else {
        ensure!(
            "facts",
            facts.is_empty(),
            "the backend has no display queries, yet: {facts:?}"
        );
    }
    h.finish(&[&c])
}

/// Case 4.
pub async fn detach_ends_with_a_final_closed<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut c = h.client();
    let att = h
        .attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    h.mark(&term, &mut [&mut c]).await?;
    h.detach(&mut c, att).await?;
    h.settle(&mut [&mut c]).await?;
    let view = view_of(&c, att)?;
    ensure!(
        "closed",
        view.transcript.last() == Some(&Applied::Closed(CloseReason::Detached)),
        "the last applied item should be Closed(Detached); the transcript is {:?}",
        view.transcript
    );

    let before = view.transcript.clone();
    h.emit(&term, b"late").await?;
    h.settle(&mut [&mut c]).await?;
    ensure!(
        "after-detach",
        view_of(&c, att)?.transcript == before,
        "output arrived after Closed(Detached)"
    );

    let request = c.next_request();
    let cursor = c
        .view(att)
        .and_then(|view| view.last_applied)
        .ok_or_else(|| crate::contract::Failure::new("harness", "nothing was applied"))?;
    h.send(
        &mut c,
        ClientMessage::Resume(Resume {
            request,
            cursor,
            instance: None,
        }),
    )
    .await?;
    h.settle(&mut [&mut c]).await?;
    ensure!(
        "resume-gap",
        c.resume_refused(att),
        "resuming an acked, detached attachment should be a gap with the request; got {:?}",
        c.controls
    );
    h.finish(&[&c])
}

/// Case 5.
pub async fn program_exit_is_final<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut c = h.client();
    let att = h
        .attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    h.mark(&term, &mut [&mut c]).await?;
    h.driver
        .backend()
        .kill(&term)
        .await
        .map_err(|error| crate::contract::Failure::new("kill", format!("kill failed: {error}")))?;
    h.settle(&mut [&mut c]).await?;
    let view = view_of(&c, att)?;
    ensure!(
        "closed",
        view.ending == Some(Ending::Closed(CloseReason::Exited)),
        "the program's exit should end the stream with Closed(Exited); got {:?}",
        view.ending
    );

    h.target.restart_backend().await?;
    h.settle(&mut [&mut c]).await?;
    let view = view_of(&c, att)?;
    ensure!(
        "no-restart",
        view.started.len() == 1,
        "a backend restart started a new epoch after the program exited ({} Started)",
        view.started.len()
    );
    h.finish(&[&c])
}

/// Case 6.
pub async fn adapter_reports_lost_only_after_giving_up<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let max = h.target.max_reopen_attempts().ok_or_else(|| {
        crate::contract::Failure::new("harness", "the target gave no reopen attempt limit")
    })?;

    let mut c = h.client();
    let giving_up = h
        .attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    h.mark(&term, &mut [&mut c]).await?;
    h.target.fail_next_opens(&term, max)?;
    h.target.fail_half(&term, 0, Half::Display)?;
    h.settle(&mut [&mut c]).await?;
    let view = view_of(&c, giving_up)?;
    ensure!(
        "lost",
        view.ending == Some(Ending::Lost)
            && view
                .transcript
                .iter()
                .filter(|i| **i == Applied::Lost)
                .count()
                == 1,
        "expected exactly one final Lost; the stream ended with {:?}",
        view.ending
    );
    ensure!(
        "lost",
        view.started.len() == 1,
        "a Started came between the failure and Lost ({} Started)",
        view.started.len()
    );

    let recovering = h
        .attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    h.target.fail_next_opens(&term, 1)?;
    h.target.fail_half(&term, 1, Half::Display)?;
    h.settle(&mut [&mut c]).await?;
    let view = view_of(&c, recovering)?;
    ensure!(
        "reopened",
        view.started.len() == 2 && view.ending.is_none(),
        "one failed reopen should give one new Started and no Lost; got {} Started, ending {:?}",
        view.started.len(),
        view.ending
    );
    ensure!(
        "reopened",
        view.fidelity == Some(Fidelity::Exact),
        "the reopened epoch is {:?}",
        view.fidelity
    );
    h.finish(&[&c])
}
