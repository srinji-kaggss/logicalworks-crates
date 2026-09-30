use lgwks_deps::tokio::sync::mpsc;

fn main() {
    let (_sender, _receiver) = mpsc::channel::<u8>(1);
}
