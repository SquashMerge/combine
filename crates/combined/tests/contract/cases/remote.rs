//! Case 11: another machine's terminals never appear, and every call about one is rejected.

use std::task::{Context, Poll};

use combine_protocol::{ErrorCode, TermId};
use combined::core::{CoreConfig, Spaces};
use combined::model::{RootKind, Space, SpaceId, TermEvent};
use combined::ports::{AttachOptions, BackendError};
use futures::stream::StreamExt;
use futures::FutureExt;

use crate::common::{request_options, FakeGitProbe};
use crate::contract::{Case, Failure, Fault, Harness, Outcome, Target};

pub fn cases<T: Target>() -> Vec<Case<T>> {
    vec![Case {
        name: "remote_terminals_never_appear_and_are_rejected",
        needs: &[Fault::RemoteTwin],
        heavy: false,
        run: |t| remote_terminals_never_appear_and_are_rejected(t).boxed_local(),
    }]
}

fn names(event: &TermEvent, id: &TermId) -> bool {
    match event {
        TermEvent::Created(info) | TermEvent::Changed(info) => info.id == *id,
        TermEvent::Exited { id: named, .. }
        | TermEvent::Lost { id: named }
        | TermEvent::Restarted { id: named } => named == id,
    }
}

/// Case 11.
pub async fn remote_terminals_never_appear_and_are_rejected<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let backend = h.driver.backend().clone();
    let mut watch = backend
        .watch()
        .await
        .map_err(|error| Failure::new("harness", format!("watch failed: {error}")))?;
    let local = h.spawn("p", 80, 24).await?;
    let twin = h.target.remote_twin(&local)?;
    let dir = h.target.scratch_dir("p");

    let listed = backend
        .list()
        .await
        .map_err(|error| Failure::new("list", format!("list failed: {error}")))?;
    ensure!(
        "remote-listed",
        listed.iter().all(|info| info.id != twin),
        "list() shows the remote terminal {}",
        twin.0
    );
    ensure!(
        "remote-listed",
        listed
            .iter()
            .any(|info| info.id == local && info.cwd == dir),
        "list() is missing the local terminal"
    );

    let mut context = Context::from_waker(futures::task::noop_waker_ref());
    let mut events = Vec::new();
    while let Poll::Ready(Some(event)) = watch.poll_next_unpin(&mut context) {
        events.push(event);
    }
    ensure!(
        "remote-watched",
        events.iter().all(|event| !names(event, &twin)),
        "watch() reported the remote terminal: {events:?}"
    );

    let attach = backend
        .attach(&twin, AttachOptions::from(request_options(80, 24, false)))
        .await;
    ensure!(
        "remote-rejected",
        matches!(attach, Err(BackendError::RemoteTerminal(_))),
        "attach of a remote terminal should be RemoteTerminal; got {:?}",
        attach.err()
    );
    let kill = backend.kill(&twin).await;
    ensure!(
        "remote-rejected",
        matches!(kill, Err(BackendError::RemoteTerminal(_))),
        "kill of a remote terminal should be RemoteTerminal; got {kill:?}"
    );

    let mut c = h.client();
    let answer = h
        .try_attach(&mut c, &twin, request_options(80, 24, false))
        .await?;
    ensure!(
        "remote-rejected",
        answer
            .as_ref()
            .is_err_and(|error| error.code == ErrorCode::UnknownTerminal),
        "attaching a remote terminal through the daemon should be unknown_terminal; got {answer:?}"
    );

    let mut spaces = Spaces::new(Box::new(FakeGitProbe::default().with_repository(&dir)));
    spaces.apply_list(&listed);
    let expected = vec![Space {
        id: SpaceId(dir),
        kind: RootKind::Repository,
        terminals: vec![local],
    }];
    ensure!(
        "spaces",
        spaces.spaces() == expected,
        "expected one space holding only the local terminal; got {:?}",
        spaces.spaces()
    );
    h.finish(&[&c])
}
