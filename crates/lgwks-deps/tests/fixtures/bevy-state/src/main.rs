use lgwks_deps::bevy_state as bevy_state;
use lgwks_deps::bevy_state::prelude::States;

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq, States)]
enum Screen {
    #[default]
    Loading,
}

fn main() {
    let _state = Screen::default();
}
