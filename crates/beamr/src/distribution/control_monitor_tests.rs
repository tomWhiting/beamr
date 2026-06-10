use super::*;

fn plane() -> (ControlPlane, Arc<RecordingMonitorSender>) {
    let sender = Arc::new(RecordingMonitorSender::new());
    let plane = ControlPlane::new(
        Atom::OK,
        Arc::clone(&sender) as Arc<dyn MonitorControlSender>,
    );
    (plane, sender)
}

#[test]
fn monitor_remote_records_and_sends_monitor_p() {
    let (plane, sender) = plane();
    let target = RemotePid::new(Atom::ERROR, 42, 7);

    let reference = plane.monitor_remote(11, target).expect("monitor sends");

    assert!(reference >= REMOTE_MONITOR_REFERENCE_START);
    assert_eq!(
        sender.drain(),
        vec![OutboundMonitorControl {
            node: Atom::ERROR,
            message: MonitorControlMessage::MonitorP {
                reference,
                watcher: RemotePid::new(Atom::OK, 11, 0),
                target,
            },
        }]
    );
}

#[test]
fn demonitor_remote_removes_record_and_sends_demonitor_p_once() {
    let (plane, sender) = plane();
    let target = RemotePid::new(Atom::ERROR, 42, 7);
    let reference = plane.monitor_remote(11, target).expect("monitor sends");
    sender.drain();

    assert!(
        plane
            .demonitor_remote(11, reference)
            .expect("demonitor sends")
    );
    assert_eq!(
        sender.drain(),
        vec![OutboundMonitorControl {
            node: Atom::ERROR,
            message: MonitorControlMessage::DemonitorP {
                reference,
                watcher: RemotePid::new(Atom::OK, 11, 0),
                target,
            },
        }]
    );
    assert!(
        !plane
            .demonitor_remote(11, reference)
            .expect("idempotent miss")
    );
}

#[test]
fn demonitor_suppresses_later_monitor_p_exit_for_inbound_registration() {
    let (plane, sender) = plane();
    let watcher = RemotePid::new(Atom::ERROR, 11, 0);
    plane.register_inbound_monitor(5, watcher, 42);

    plane.remove_inbound_monitor(5, watcher, 42);
    let drained = plane.collect_inbound_for_target(42);

    assert!(drained.is_empty());
    assert!(sender.drain().is_empty());
}

#[test]
fn node_down_removes_inbound_watchers_for_failed_node() {
    let (plane, _sender) = plane();
    plane.register_inbound_monitor(5, RemotePid::new(Atom::ERROR, 11, 0), 42);
    plane.register_inbound_monitor(6, RemotePid::new(Atom::OK, 12, 0), 42);

    plane.remove_inbound_for_watcher_node(Atom::ERROR);

    assert_eq!(
        plane.collect_inbound_for_target(42),
        vec![InboundRemoteMonitor {
            watcher: RemotePid::new(Atom::OK, 12, 0),
            reference: 6,
            target_pid: 42,
        }]
    );
}

#[test]
fn collect_outbound_for_node_drains_matching_monitors() {
    let (plane, _sender) = plane();
    let target_a = RemotePid::new(Atom::ERROR, 42, 0);
    let target_b = RemotePid::new(Atom::OK, 99, 0);
    let ref_a = plane.monitor_remote(1, target_a).expect("monitor a sends");
    let _ref_b = plane.monitor_remote(2, target_b).expect("monitor b sends");

    let collected = plane.collect_outbound_for_node(Atom::ERROR);

    assert_eq!(collected.len(), 1);
    assert_eq!(collected[0].reference, ref_a);
    assert_eq!(collected[0].target, target_a);

    // Second call for same node yields nothing.
    assert!(plane.collect_outbound_for_node(Atom::ERROR).is_empty());
}

#[test]
fn take_outbound_for_exit_removes_entry() {
    let (plane, _sender) = plane();
    let target = RemotePid::new(Atom::ERROR, 42, 7);
    let reference = plane.monitor_remote(11, target).expect("monitor sends");

    let taken = plane.take_outbound_for_exit(reference);

    assert!(taken.is_some());
    let monitor = taken.expect("taken");
    assert_eq!(monitor.watcher_pid, 11);
    assert_eq!(monitor.target, target);

    // Second take is None.
    assert!(plane.take_outbound_for_exit(reference).is_none());
}

#[test]
fn send_monitor_exit_emits_monitor_p_exit() {
    let (plane, sender) = plane();
    let watcher = RemotePid::new(Atom::ERROR, 11, 0);
    let inbound = InboundRemoteMonitor {
        watcher,
        reference: 5,
        target_pid: 42,
    };

    plane
        .send_monitor_exit(inbound, ExitReason::Normal)
        .expect("exit sends");

    let sent = sender.drain();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].node, Atom::ERROR);
    match sent[0].message {
        MonitorControlMessage::MonitorPExit {
            reference,
            target,
            reason,
        } => {
            assert_eq!(reference, 5);
            assert_eq!(target, RemotePid::new(Atom::OK, 42, 0));
            assert_eq!(reason, ExitReason::Normal);
        }
        _ => panic!("expected MonitorPExit"),
    }
}

#[test]
fn register_inbound_deduplicates_matching_triple() {
    let (plane, _sender) = plane();
    let watcher = RemotePid::new(Atom::ERROR, 11, 0);
    plane.register_inbound_monitor(5, watcher, 42);
    plane.register_inbound_monitor(5, watcher, 42);

    let collected = plane.collect_inbound_for_target(42);
    assert_eq!(collected.len(), 1);
}
