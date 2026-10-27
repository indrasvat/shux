use std::cmp::Reverse;

use crate::model::State;

/// How much a state needs the user, highest first: blocked > error > done >
/// working > idle (kitty's and tuios's order).
pub(crate) fn urgency(state: State) -> u8 {
    match state {
        State::Blocked => 4,
        State::Error => 3,
        State::Done => 2,
        State::Working => 1,
        State::Idle => 0,
    }
}

/// Sort key, smallest first: most urgent, then the root record, then the
/// most recently changed.
pub(crate) fn order_key(state: State, is_root: bool, updated: u64) -> impl Ord {
    (Reverse(urgency(state)), !is_root, Reverse(updated))
}
