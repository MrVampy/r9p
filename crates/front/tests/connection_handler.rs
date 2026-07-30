use front::{Front, FrontConnectionHandler};
use r9p::server::ConnectionHandler;

fn assert_connection_handler<T: ConnectionHandler>(_handler: &T) {}

#[test]
fn front_exposes_its_cancellation_aware_connection_handler() {
    let front = Front::new();
    let handler: FrontConnectionHandler = front.connection_handler();

    assert_connection_handler(&handler);
}
