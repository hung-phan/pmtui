use super::*;

mod locking;
mod paths;

// Retry budget for assertions that expect a lock to become acquirable after its holder drops it.
// Sibling tests can fork, briefly preserving an inherited lease fd until exec closes it.
const REACQUIRE_TRIES: u32 = 50;
const REACQUIRE_DELAY: std::time::Duration = std::time::Duration::from_millis(10);

fn wait_until_free(path: &Path) {
    for _ in 0..REACQUIRE_TRIES {
        if is_held(path).unwrap() == Some(false) {
            return;
        }
        std::thread::sleep(REACQUIRE_DELAY);
    }
    panic!("lock at {path:?} never became free within the retry budget");
}
