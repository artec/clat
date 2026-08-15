use clat::{
    CancelToken, EventSink, FinishReason, Model, ModelError, ModelEvent, ModelEventSink, ModelItem,
    ModelRequest, ModelResponse, Project, Run, RunEvent, SafeByDefault, Tool, ToolCall,
    ToolDefinition, ToolEffect, ToolError, ToolRegistry, Usage, register_native_read_tools,
};
use serde_json::{Value, json};
use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

const NAME: &str = "clat";
const TAGLINE: &str = "command line agent";

fn main() -> ExitCode {
    run(env::args().skip(1))
}

fn run<I>(mut args: I) -> ExitCode
where
    I: Iterator<Item = String>,
{
    match args.next().as_deref() {
        None => run_tui(),
        Some("demo") => run_demo(),
        Some("upgrade") => run_upgrade(args.next().as_deref() == Some("--check")),
        Some("-V" | "--version") => {
            println!("{NAME} {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("-h" | "--help") => {
            print_help();
            ExitCode::SUCCESS
        }
        Some(command) => {
            eprintln!("clat: unknown command or argument: {command}");
            eprintln!("Run `clat --help` for usage.");
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    println!("{NAME} — {TAGLINE}");
    println!();
    println!("Usage: clat [COMMAND]");
    println!();
    println!("Running `clat` with no command opens the interactive TUI.");
    println!("Inside the TUI, use `/model` to configure model parameters and credentials.");
    println!();
    println!("Commands:");
    println!("  demo             Run the deterministic model → tool → model loop");
    println!("  upgrade          Upgrade to the latest GitHub release");
    println!();
    println!("Options:");
    println!("  -h, --help       Print help");
    println!("  -V, --version    Print version");
}

fn run_tui() -> ExitCode {
    let project = match Project::current() {
        Ok(project) => project,
        Err(error) => {
            eprintln!("clat: could not determine current project: {error}");
            return ExitCode::FAILURE;
        }
    };

    match clat::tui::run(project) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("clat: TUI failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_demo() -> ExitCode {
    let project = match Project::current() {
        Ok(project) => project,
        Err(error) => {
            eprintln!("clat: could not determine current project: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut model = DemoModel;
    execute_demo(&mut model, &project, "prove the agent loop works".into())
}

/// `clat upgrade [--check]`：检查并安装 GitHub 最新 release。
/// `--check` 只报告不安装；已是最新输出提示并以 0 退出。
fn run_upgrade(check_only: bool) -> ExitCode {
    use clat::upgrade::UpgradeOutcome;
    match clat::upgrade::upgrade(check_only) {
        Ok(UpgradeOutcome::UpToDate { latest }) => {
            println!(
                "{NAME} {} is up to date (latest release {latest})",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        Ok(UpgradeOutcome::Available { tag }) => {
            println!("{NAME} {} → {tag} available", env!("CARGO_PKG_VERSION"));
            println!("Run `clat upgrade` to install {tag}.");
            ExitCode::SUCCESS
        }
        Ok(UpgradeOutcome::Installed { tag }) => {
            println!("{NAME} upgraded to {tag}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("clat: upgrade failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn execute_demo(model: &mut dyn Model, project: &Project, prompt: String) -> ExitCode {
    let mut tools = ToolRegistry::new();
    register_native_read_tools(&mut tools);
    tools.register(EchoTool);

    let permissions = SafeByDefault;
    let mut sink = DemoEventSink::default();
    let result = Run::new(model, &tools, &permissions, project)
        .with_instructions(
            "You are CLAT, a command line agent. Use provided tools when they help satisfy the user request.",
        )
        .execute(prompt, &mut sink);

    match result {
        Ok(output) => {
            if !sink.streamed_text {
                println!("{}", output.text);
            } else {
                println!();
            }
            eprintln!(
                "[{} turns, {} input tokens, {} output tokens]",
                output.turns, output.usage.input_tokens, output.usage.output_tokens
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("clat: {error}");
            ExitCode::FAILURE
        }
    }
}

struct EchoTool;

impl Tool for EchoTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "echo".into(),
            description: "Echo text back to the model. Useful for validating the tool loop.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "Text to echo"}
                },
                "required": ["text"],
                "additionalProperties": false
            }),
            effect: ToolEffect::Pure,
            strict: true,
        }
    }

    fn invoke(
        &self,
        arguments: &Value,
        _project: &Project,
        _cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        arguments
            .get("text")
            .and_then(Value::as_str)
            .map(|text| Value::String(text.to_owned()))
            .ok_or_else(|| ToolError::new("echo requires a string `text` argument"))
    }
}

struct DemoModel;

impl Model for DemoModel {
    fn provider(&self) -> &str {
        "demo"
    }

    fn model_id(&self) -> &str {
        "deterministic"
    }

    fn stream(
        &mut self,
        request: ModelRequest<'_>,
        events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        if let Some(ModelItem::ToolResult(result)) = request.items.last() {
            let text = format!(
                "Agent loop completed successfully. Tool returned: {}",
                result.output.as_str().unwrap_or_default()
            );
            events.emit(ModelEvent::TextDelta {
                delta: text.clone(),
            });
            events.emit(ModelEvent::ResponseCompleted {
                finish_reason: FinishReason::Completed,
            });
            return Ok(ModelResponse {
                text,
                tool_calls: vec![],
                finish_reason: FinishReason::Completed,
                usage: Some(Usage::default()),
                provider_response_id: None,
                provider_state: vec![],
                reasoning: None,
            });
        }

        let call = ToolCall {
            id: "demo-call-1".into(),
            name: "echo".into(),
            arguments: json!({"text": "model → tool → model"}),
        };
        events.emit(ModelEvent::ToolCallCompleted { call: call.clone() });
        events.emit(ModelEvent::ResponseCompleted {
            finish_reason: FinishReason::ToolCalls,
        });
        Ok(ModelResponse {
            text: String::new(),
            tool_calls: vec![call],
            finish_reason: FinishReason::ToolCalls,
            usage: Some(Usage::default()),
            provider_response_id: None,
            provider_state: vec![],
            reasoning: None,
        })
    }
}

#[derive(Default)]
struct DemoEventSink {
    streamed_text: bool,
}

impl EventSink for DemoEventSink {
    fn emit(&mut self, event: RunEvent) {
        match event {
            RunEvent::ModelRequested {
                turn,
                provider,
                model,
            } => eprintln!("● {provider}/{model} turn {turn}"),
            RunEvent::ModelStream {
                event: ModelEvent::TextDelta { delta },
                ..
            }
            | RunEvent::ModelStream {
                event: ModelEvent::RefusalDelta { delta },
                ..
            } => {
                self.streamed_text = true;
                print!("{delta}");
                let _ = io::stdout().flush();
            }
            RunEvent::ToolRequested { call } => {
                eprintln!("\n● tool {} {}", call.name, call.arguments)
            }
            RunEvent::PermissionChecked { decision, .. } => eprintln!("● permission {decision:?}"),
            RunEvent::PermissionDenied { tool, reason } => {
                eprintln!("● permission denied {tool}: {reason}")
            }
            RunEvent::ToolFinished { result } => {
                if result.is_error {
                    eprintln!("● tool error {}", result.output);
                } else {
                    eprintln!("● tool result {}", result.output);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_argument_fails() {
        let code = run(["unknown".to_owned()].into_iter());
        assert_eq!(code, ExitCode::from(2));
    }

    #[test]
    fn help_succeeds() {
        let code = run(["--help".to_owned()].into_iter());
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn demo_command_executes_agent_loop() {
        let code = run(["demo".to_owned()].into_iter());
        assert_eq!(code, ExitCode::SUCCESS);
    }
}
