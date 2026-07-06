use loom::{
	sync::{
		Arc, Mutex, MutexGuard,
		atomic::{AtomicUsize, Ordering},
	},
	thread,
};

const LOW_ADDRESS: usize = 0x10;
const HIGH_ADDRESS: usize = 0x20;

struct CriticalSection<'a> {
	_guards: Vec<MutexGuard<'a, ()>>,
}

fn begin2_model<'a>(
	left: usize,
	right: usize,
	low_lock: &'a Mutex<()>,
	high_lock: &'a Mutex<()>,
) -> CriticalSection<'a> {
	let mut keys = [left, right];
	keys.sort_unstable();

	let mut guards = Vec::with_capacity(2);
	let mut previous = None;
	for key in keys {
		if previous == Some(key) {
			continue;
		}
		previous = Some(key);

		let lock = match key {
			LOW_ADDRESS => low_lock,
			HIGH_ADDRESS => high_lock,
			_ => unreachable!("test model only has two object locks"),
		};
		guards.push(lock.lock().expect("loom mutex should not be poisoned"));
		thread::yield_now();
	}

	CriticalSection { _guards: guards }
}

fn enter_two_object_section(
	left: usize,
	right: usize,
	low_lock: Arc<Mutex<()>>,
	high_lock: Arc<Mutex<()>>,
	inside: Arc<AtomicUsize>,
	completed: Arc<AtomicUsize>,
) {
	let _critical_section = begin2_model(left, right, &low_lock, &high_lock);

	assert_eq!(
		inside.fetch_add(1, Ordering::AcqRel),
		0,
		"two threads held the same two-object critical section concurrently"
	);
	thread::yield_now();
	inside.fetch_sub(1, Ordering::AcqRel);
	completed.fetch_add(1, Ordering::AcqRel);
}

#[test]
fn begin2_address_ordering_prevents_opposite_order_deadlock() {
	loom::model(|| {
		let low_lock = Arc::new(Mutex::new(()));
		let high_lock = Arc::new(Mutex::new(()));
		let inside = Arc::new(AtomicUsize::new(0));
		let completed = Arc::new(AtomicUsize::new(0));

		let first = {
			let low_lock = Arc::clone(&low_lock);
			let high_lock = Arc::clone(&high_lock);
			let inside = Arc::clone(&inside);
			let completed = Arc::clone(&completed);
			thread::spawn(move || {
				enter_two_object_section(
					LOW_ADDRESS,
					HIGH_ADDRESS,
					low_lock,
					high_lock,
					inside,
					completed,
				);
			})
		};

		let second = {
			let low_lock = Arc::clone(&low_lock);
			let high_lock = Arc::clone(&high_lock);
			let inside = Arc::clone(&inside);
			let completed = Arc::clone(&completed);
			thread::spawn(move || {
				enter_two_object_section(
					HIGH_ADDRESS,
					LOW_ADDRESS,
					low_lock,
					high_lock,
					inside,
					completed,
				);
			})
		};

		first
			.join()
			.expect("first critical-section thread panicked");
		second
			.join()
			.expect("second critical-section thread panicked");

		assert_eq!(completed.load(Ordering::Acquire), 2);
		assert_eq!(inside.load(Ordering::Acquire), 0);
	});
}
