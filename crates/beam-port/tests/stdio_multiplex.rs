use std::{
    io::{BufRead, BufReader, Write},
    process::{ChildStdin, Command, Stdio},
};

#[test]
fn pending_front_request_does_not_block_projection_updates() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_r9p-beam-port"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn beam port");
    let mut stdin = child.stdin.take().expect("beam port stdin");
    let stdout = child.stdout.take().expect("beam port stdout");
    let mut stdout = BufReader::new(stdout);

    request(&mut stdin, 1, "front-new");
    let (request_id, response) = read_response(&mut stdout);
    assert_eq!(request_id, 1);
    let front_response = response.expect("front-new response");
    let front_id = front_response
        .strip_prefix("front\t")
        .expect("front id response")
        .parse::<u64>()
        .expect("numeric front id");

    request(
        &mut stdin,
        2,
        &format!("front-next-request\t{front_id}\t500"),
    );
    request(
        &mut stdin,
        3,
        &format!("front-set\t{front_id}\t737461747573\t7265616479"),
    );

    let (request_id, response) = read_response(&mut stdout);
    assert_eq!(
        request_id, 3,
        "projection update must pass a pending request intake"
    );
    assert_eq!(response, Ok("front-set".to_string()));

    let (request_id, response) = read_response(&mut stdout);
    assert_eq!(request_id, 2);
    assert_eq!(response, Ok("front-timeout".to_string()));

    request(&mut stdin, 4, &format!("front-stop\t{front_id}"));
    let (request_id, response) = read_response(&mut stdout);
    assert_eq!(request_id, 4);
    assert_eq!(response, Ok("front-stop".to_string()));

    drop(stdin);
    assert!(child.wait().expect("wait for beam port").success());
}

#[test]
fn pending_ordinary_rpc_does_not_block_an_independent_stat() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_r9p-beam-port"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn beam port");
    let mut stdin = child.stdin.take().expect("beam port stdin");
    let stdout = child.stdout.take().expect("beam port stdout");
    let mut stdout = BufReader::new(stdout);

    request(&mut stdin, 1, "front-new");
    let (_, front) = read_response(&mut stdout);
    let front_id = front
        .expect("front-new response")
        .strip_prefix("front\t")
        .expect("front id response")
        .parse::<u64>()
        .expect("numeric front id");

    request(
        &mut stdin,
        2,
        &format!(
            "front-register-rpc\t{front_id}\t{}",
            encode_hex("declaration")
        ),
    );
    assert_eq!(read_response(&mut stdout).0, 2);

    request(
        &mut stdin,
        3,
        &format!("front-serve-tcp\t{front_id}\t{}", encode_hex("127.0.0.1:0")),
    );
    let (_, serve) = read_response(&mut stdout);
    let address = serve
        .expect("front serve response")
        .strip_prefix("front-serve-tcp\t")
        .map(decode_hex)
        .expect("front address response");

    request(
        &mut stdin,
        4,
        &client_command(
            "rpc",
            &address,
            &[encode_hex("declaration"), encode_hex("compile this")],
        ),
    );
    request(
        &mut stdin,
        5,
        &format!("front-next-request\t{front_id}\t5000"),
    );
    let (request_id, pending) = read_response(&mut stdout);
    assert_eq!(request_id, 5);
    let pending = pending.expect("pending RPC request");
    let fields = pending.split('\t').collect::<Vec<_>>();
    assert_eq!(fields[0], "front-request");

    request(
        &mut stdin,
        6,
        &client_command("stat", &address, &[encode_hex("declaration")]),
    );
    let (request_id, stat) = read_response(&mut stdout);
    assert_eq!(request_id, 6, "stat must pass the pending ordinary RPC");
    assert!(stat.expect("stat response").starts_with("stat\t"));

    request(
        &mut stdin,
        7,
        &format!(
            "front-complete-request\t{front_id}\t{}\t{}\t{}",
            fields[1],
            fields[2],
            encode_hex("compiled")
        ),
    );
    let first = read_response(&mut stdout);
    let second = read_response(&mut stdout);
    let mut completed = [first, second];
    completed.sort_by_key(|response| response.0);
    assert_eq!(completed[0], (4, Ok("rpc\t8\t636f6d70696c6564".to_string())));
    assert_eq!(completed[1], (7, Ok("front-complete-request".to_string())));

    request(&mut stdin, 8, &format!("front-stop\t{front_id}"));
    assert_eq!(read_response(&mut stdout).0, 8);
    drop(stdin);
    assert!(child.wait().expect("wait for beam port").success());
}

fn request(stdin: &mut ChildStdin, request_id: u64, command: &str) {
    writeln!(stdin, "{request_id}\t{command}").expect("write beam port request");
    stdin.flush().expect("flush beam port request");
}

fn read_response(reader: &mut impl BufRead) -> (u64, Result<String, String>) {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("read beam port response");
    let fields = line.trim_end().splitn(3, '\t').collect::<Vec<_>>();
    assert_eq!(fields.len(), 3, "tagged beam port response");
    let request_id = fields[0].parse::<u64>().expect("response request id");
    let payload = decode_hex(fields[2]);
    let response = match fields[1] {
        "ok" => Ok(payload),
        "error" => Err(payload),
        other => panic!("unexpected response status: {other}"),
    };
    (request_id, response)
}

fn decode_hex(value: &str) -> String {
    assert_eq!(value.len() % 2, 0, "even hex length");
    let bytes = value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_value(pair[0]);
            let low = hex_value(pair[1]);
            (high << 4) | low
        })
        .collect::<Vec<_>>();
    String::from_utf8(bytes).expect("UTF-8 response payload")
}

fn encode_hex(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn client_command(operation: &str, address: &str, arguments: &[String]) -> String {
    format!(
        "{operation}\t{}\t{}\t{}\t65536\t\t\t{}",
        encode_hex(address),
        encode_hex("codex"),
        encode_hex("/"),
        arguments.join("\t")
    )
}

fn hex_value(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        b'A'..=b'F' => value - b'A' + 10,
        _ => panic!("invalid hex digit"),
    }
}
