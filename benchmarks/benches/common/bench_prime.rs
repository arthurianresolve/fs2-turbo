macro_rules! iter_primed {
    ($bencher:expr, $workload:expr, $operation:expr $(,)?) => {{
        let mut operation = $operation;
        $crate::bench_support::record_prime_once($workload, &mut operation);
        $bencher.iter(operation);
    }};
}

pub(crate) use iter_primed;
