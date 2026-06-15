use std::sync::Arc;
use std::time::Duration;

use sonda_core::schedule::gate_bus::{gate_edge_channel, GateEdge};
use tokio::sync::Barrier;
use tokio::task::yield_now;

const DEFAULT_N: usize = 10_000;
const HEAVY_N: usize = 1_000_000;
const OBSERVE_DEADLINE: Duration = Duration::from_millis(50);

async fn pattern_a_run(iterations: usize) {
    for _ in 0..iterations {
        let (tx, mut rx) = gate_edge_channel();
        let final_edge = GateEdge::WhileClose;
        let pub_handle = tokio::spawn(async move {
            tx.send(GateEdge::WhileOpen);
            yield_now().await;
            tx.send(GateEdge::WhileClose);
            yield_now().await;
            tx.send(GateEdge::WhileOpen);
            yield_now().await;
            tx.send(final_edge);
            yield_now().await;
        });

        pub_handle.await.expect("publisher task");

        let observed = loop {
            if let Some(edge) = rx.try_recv() {
                if edge == final_edge {
                    break Some(edge);
                }
                continue;
            }
            match tokio::time::timeout(OBSERVE_DEADLINE, rx.wait_for_change()).await {
                Ok(()) => continue,
                Err(_) => break rx.try_recv(),
            }
        };

        assert_eq!(
            observed,
            Some(final_edge),
            "pattern A: final-state observation lost the last edge"
        );
    }
}

async fn pattern_b_run(iterations: usize) {
    for _ in 0..iterations {
        let (tx, mut rx) = gate_edge_channel();
        let barrier = Arc::new(Barrier::new(2));

        let sub_barrier = Arc::clone(&barrier);
        let sub = tokio::spawn(async move {
            {
                let notified = rx.wait_for_change();
                tokio::pin!(notified);
                sub_barrier.wait().await;
                tokio::time::timeout(OBSERVE_DEADLINE, &mut notified)
                    .await
                    .expect("pattern B: wait_for_change did not resolve within deadline");
            }
            rx.try_recv()
        });

        let pub_barrier = Arc::clone(&barrier);
        let pubr = tokio::spawn(async move {
            pub_barrier.wait().await;
            tx.send(GateEdge::WhileOpen);
            yield_now().await;
        });

        pubr.await.expect("publisher task");
        let observed = sub.await.expect("subscriber task");
        assert_eq!(
            observed,
            Some(GateEdge::WhileOpen),
            "pattern B: wake-up lost the only published edge"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pattern_a_final_state_observation_default_n() {
    pattern_a_run(DEFAULT_N).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pattern_b_inverted_control_wakeup_default_n() {
    pattern_b_run(DEFAULT_N).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn pattern_a_final_state_observation_heavy_n() {
    pattern_a_run(HEAVY_N).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn pattern_b_inverted_control_wakeup_heavy_n() {
    pattern_b_run(HEAVY_N).await;
}
