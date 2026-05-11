use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;

pub mod codex;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Log in with ChatGPT through Codex and save credentials for future AI commands.
    Auth,
    /// Show whether Codex is authenticated with ChatGPT.
    Status,
    /// Summarize a transcript using the saved Codex ChatGPT login.
    Summarize {
        /// Transcript text file.
        #[arg(short, long)]
        input: PathBuf,

        /// Extra instruction for the summary, such as audience or format.
        #[arg(short = 'n', long)]
        instruction: Option<String>,

        /// Codex model to use. Defaults to the user's Codex config.
        #[arg(long)]
        model: Option<String>,
    },
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Auth => codex::authenticate(),
        Command::Status => {
            let status = codex::auth_status()?;
            println!("{status}");
            Ok(())
        }
        Command::Summarize {
            input,
            instruction,
            model,
        } => {
            codex::ensure_chatgpt_login()?;
            let transcript = codex::read_transcript(&input)?;
            let summary = codex::summarize(codex::SummaryRequest {
                transcript: &transcript,
                instruction: instruction.as_deref(),
                model: model.as_deref(),
            })?;
            println!("{summary}");
            Ok(())
        }
    }
}
