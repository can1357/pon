use loom::{
	sync::{
		Arc,
		atomic::{AtomicBool, AtomicUsize, Ordering},
	},
	thread,
};

const WHITE: usize = 0;
const GREY: usize = 1;

#[derive(Debug)]
struct BarrierModel {
	concurrent_marking: AtomicBool,
	color:              AtomicUsize,
	enqueue_winners:    AtomicUsize,
	records:            AtomicUsize,
}

impl BarrierModel {
	fn during_concurrent_mark() -> Self {
		Self {
			concurrent_marking: AtomicBool::new(true),
			color:              AtomicUsize::new(WHITE),
			enqueue_winners:    AtomicUsize::new(0),
			records:            AtomicUsize::new(0),
		}
	}

	fn shade(&self) -> bool {
		let won = self
			.color
			.compare_exchange(WHITE, GREY, Ordering::AcqRel, Ordering::Acquire)
			.is_ok();
		if won {
			self.enqueue_winners.fetch_add(1, Ordering::AcqRel);
		}
		won
	}

	fn record_write_barrier(&self) {
		if self.concurrent_marking.load(Ordering::Acquire) {
			self.records.fetch_add(1, Ordering::AcqRel);
			self.shade();
		}
	}
}

#[test]
fn concurrent_shade_and_write_barrier_mark_once() {
	loom::model(|| {
		let model = Arc::new(BarrierModel::during_concurrent_mark());

		let direct_shade = Arc::clone(&model);
		let direct_thread = thread::spawn(move || direct_shade.shade());

		let barrier_record = Arc::clone(&model);
		let barrier_thread = thread::spawn(move || barrier_record.record_write_barrier());

		let _direct_thread_won = direct_thread.join().expect("direct shade thread panicked");
		barrier_thread
			.join()
			.expect("write-barrier record thread panicked");

		assert_eq!(model.records.load(Ordering::Acquire), 1);
		assert_eq!(model.color.load(Ordering::Acquire), GREY);

		let enqueue_winners = model.enqueue_winners.load(Ordering::Acquire);
		assert_eq!(enqueue_winners, 1, "exactly one shade transition may enqueue the object");
	});
}
