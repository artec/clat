//! Native stdio fixture for the Windows ACP integration test.
use std::io::{self, BufRead, Write};

fn expect_input(input: &mut impl BufRead, expected: &str) -> io::Result<()> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    if !line.contains(expected) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected {expected}"),
        ));
    }
    Ok(())
}

fn send(output: &mut impl Write, frame: &str) -> io::Result<()> {
    writeln!(output, "{frame}")?;
    output.flush()
}

fn main() -> io::Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let read_request = std::env::args().nth(1).expect("read callback frame");

    expect_input(&mut input, r#""method":"initialize""#)?;
    send(
        &mut output,
        r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}"#,
    )?;
    expect_input(&mut input, r#""method":"session/new""#)?;
    send(
        &mut output,
        r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"external"}}"#,
    )?;
    expect_input(&mut input, r#""method":"session/prompt""#)?;
    send(&mut output, &read_request)?;
    expect_input(&mut input, r#""content":"fixture text""#)?;
    send(
        &mut output,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"external","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"verified"}}}}"#,
    )?;
    send(
        &mut output,
        r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#,
    )?;
    io::copy(&mut input, &mut io::sink())?;
    Ok(())
}
