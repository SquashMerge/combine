//! Who may type into a terminal, for the attachments that share it.

use std::collections::BTreeMap;

use combine_protocol::{AttachmentId, OwnershipGeneration};

use super::machine::OwnershipState;

/// The state after a change, and the owner it gave the terminal to, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipChange {
    pub state: OwnershipState,
    pub new_owner: Option<AttachmentId>,
}

#[derive(Debug)]
struct Member {
    watch_only: bool,
    eligible: bool,
    focus_order: Option<u64>,
}

/// One terminal's members and its owner.
///
/// Every change to the owner takes a new generation, including a change to no owner. Core
/// supplies the generations from its one counter.
#[derive(Debug)]
pub struct TerminalOwnership {
    state: OwnershipState,
    members: BTreeMap<AttachmentId, Member>,
}

impl TerminalOwnership {
    pub fn new(generation: OwnershipGeneration) -> Self {
        TerminalOwnership {
            state: OwnershipState {
                generation,
                owner: None,
            },
            members: BTreeMap::new(),
        }
    }

    /// Adds an eligible member. Never changes the owner.
    pub fn join(&mut self, id: AttachmentId, watch_only: bool) {
        self.members.insert(
            id,
            Member {
                watch_only,
                eligible: true,
                focus_order: None,
            },
        );
    }

    /// Records when the member last had focus. A larger `order` is more recent.
    pub fn focus(&mut self, id: AttachmentId, order: u64) {
        if let Some(member) = self.members.get_mut(&id) {
            member.focus_order = Some(order);
        }
    }

    /// Makes `id` the owner under `generation`.
    pub fn grant(&mut self, id: AttachmentId, generation: OwnershipGeneration) -> OwnershipChange {
        self.state = OwnershipState {
            generation,
            owner: Some(id),
        };
        OwnershipChange {
            state: self.state.clone(),
            new_owner: Some(id),
        }
    }

    /// Marks whether the member may own. Becoming eligible never grants ownership; becoming
    /// ineligible while owning hands the terminal off, skipping members `leaving` names.
    pub fn set_eligible(
        &mut self,
        id: AttachmentId,
        eligible: bool,
        leaving: &dyn Fn(AttachmentId) -> bool,
        next: &mut dyn FnMut() -> OwnershipGeneration,
    ) -> Option<OwnershipChange> {
        self.members.get_mut(&id)?.eligible = eligible;
        if eligible || self.state.owner != Some(id) {
            return None;
        }
        Some(self.hand_off(id, leaving, next))
    }

    /// Removes the member, handing the terminal off if it owned it, skipping members `leaving`
    /// names.
    pub fn leave(
        &mut self,
        id: AttachmentId,
        leaving: &dyn Fn(AttachmentId) -> bool,
        next: &mut dyn FnMut() -> OwnershipGeneration,
    ) -> Option<OwnershipChange> {
        self.members.remove(&id)?;
        if self.state.owner != Some(id) {
            return None;
        }
        Some(self.hand_off(id, leaving, next))
    }

    /// Passes ownership from `from` to the most recently focused member that may type and that
    /// `leaving` doesn't name, or to nobody.
    fn hand_off(
        &mut self,
        from: AttachmentId,
        leaving: &dyn Fn(AttachmentId) -> bool,
        next: &mut dyn FnMut() -> OwnershipGeneration,
    ) -> OwnershipChange {
        let heir = self
            .members
            .iter()
            .filter(|(id, member)| {
                **id != from && member.eligible && !member.watch_only && !leaving(**id)
            })
            .filter_map(|(id, member)| member.focus_order.map(|order| (order, *id)))
            .max()
            .map(|(_, id)| id);
        self.state = OwnershipState {
            generation: next(),
            owner: heir,
        };
        OwnershipChange {
            state: self.state.clone(),
            new_owner: heir,
        }
    }

    pub fn state(&self) -> &OwnershipState {
        &self.state
    }

    pub fn is_owner(&self, id: AttachmentId) -> bool {
        self.state.owner == Some(id)
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }
}
