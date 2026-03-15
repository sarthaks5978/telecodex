use super::presentation::send_markdown_message;
use super::*;
use tokio::{sync::oneshot, time::timeout};

#[derive(Clone)]
pub(super) struct PendingCodexLogin {
    pub(super) id: String,
    pub(super) prompt: crate::codex::CodexDeviceAuthPrompt,
    pub(super) cancel: CancellationToken,
}

impl App {
    pub(super) async fn ensure_codex_authenticated(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
    ) -> Result<bool> {
        let status = self.shared.codex.auth_status().await?;
        if status.authenticated {
            return Ok(true);
        }
        let pending = self.shared.pending_codex_login.lock().await.clone();
        match pending {
            Some(pending) => {
                send_markdown_message(
                    &self.shared.telegram,
                    chat_id,
                    thread_id,
                    &format_device_auth_message(&pending.prompt, true),
                    None,
                )
                .await?;
            }
            None => {
                send_markdown_message(
                    &self.shared.telegram,
                    chat_id,
                    thread_id,
                    &codex_login_required_message(),
                    None,
                )
                .await?;
            }
        }
        Ok(false)
    }

    pub(super) async fn handle_login_command(&self, message: &Message) -> Result<()> {
        let status = self.shared.codex.auth_status().await?;
        if status.authenticated {
            let detail = if status.detail.is_empty() {
                "Codex is already logged in.".to_string()
            } else {
                format!("Codex is already logged in.\n\n{}", status.detail)
            };
            self.send_status(message.chat.id, message.message_thread_id, &detail)
                .await?;
            return Ok(());
        }

        if let Some(message_text) = active_login_backoff_message(&self.shared).await {
            send_markdown_message(
                &self.shared.telegram,
                message.chat.id,
                message.message_thread_id,
                &message_text,
                None,
            )
            .await?;
            return Ok(());
        }

        if let Some(pending) = self.shared.pending_codex_login.lock().await.clone() {
            send_markdown_message(
                &self.shared.telegram,
                message.chat.id,
                message.message_thread_id,
                &format_device_auth_message(&pending.prompt, true),
                None,
            )
            .await?;
            return Ok(());
        }

        let login_id = Uuid::now_v7().to_string();
        let cancel = CancellationToken::new();
        let (prompt_tx, prompt_rx) = oneshot::channel();
        let shared = self.shared.clone();
        let chat_id = message.chat.id;
        let thread_id = message.message_thread_id;
        let task_login_id = login_id.clone();
        let task_cancel = cancel.clone();

        tokio::spawn(async move {
            if let Err(error) = run_device_auth_login(
                shared.clone(),
                chat_id,
                thread_id,
                task_login_id,
                task_cancel,
                prompt_tx,
            )
            .await
            {
                tracing::warn!("codex device login task failed: {error:#}");
            }
        });

        let prompt = match timeout(Duration::from_secs(12), prompt_rx).await {
            Ok(Ok(Ok(prompt))) => prompt,
            Ok(Ok(Err(error))) => {
                handle_login_start_failure(&self.shared, message, error).await?;
                return Ok(());
            }
            Ok(Err(_)) => {
                handle_login_start_failure(
                    &self.shared,
                    message,
                    anyhow!("codex login task dropped before sending device code"),
                )
                .await?;
                return Ok(());
            }
            Err(_) => {
                cancel.cancel();
                handle_login_start_failure(
                    &self.shared,
                    message,
                    anyhow!("timed out waiting for Codex device code"),
                )
                .await?;
                return Ok(());
            }
        };

        *self.shared.pending_codex_login.lock().await = Some(PendingCodexLogin {
            id: login_id,
            prompt: prompt.clone(),
            cancel,
        });
        send_markdown_message(
            &self.shared.telegram,
            message.chat.id,
            message.message_thread_id,
            &format_device_auth_message(&prompt, false),
            None,
        )
        .await?;
        Ok(())
    }

    pub(super) async fn handle_logout_command(&self, message: &Message) -> Result<()> {
        if let Some(pending) = self.shared.pending_codex_login.lock().await.take() {
            pending.cancel.cancel();
        }
        let detail = self.shared.codex.logout().await?;
        let body = if detail.is_empty() {
            "Codex credentials removed.".to_string()
        } else {
            format!("Codex logout completed.\n\n{detail}")
        };
        self.send_status(message.chat.id, message.message_thread_id, &body)
            .await?;
        Ok(())
    }
}

const CODEX_LOGIN_BACKOFF_SECONDS: u64 = 60;

async fn run_device_auth_login(
    shared: Arc<AppShared>,
    chat_id: i64,
    thread_id: Option<i64>,
    login_id: String,
    cancel: CancellationToken,
    prompt_tx: oneshot::Sender<Result<crate::codex::CodexDeviceAuthPrompt>>,
) -> Result<()> {
    let mut login = match shared.codex.start_device_auth().await {
        Ok(login) => login,
        Err(error) => {
            let _ = prompt_tx.send(Err(error));
            return Ok(());
        }
    };

    let prompt = match login.read_prompt().await {
        Ok(prompt) => prompt,
        Err(error) => {
            let _ = prompt_tx.send(Err(error));
            return Ok(());
        }
    };
    let _ = prompt_tx.send(Ok(prompt.clone()));

    let completion = login.wait(cancel.clone()).await;
    clear_pending_login(&shared, &login_id).await;

    match completion {
        Ok(_) => {
            if cancel.is_cancelled() {
                return Ok(());
            }
            let status = shared.codex.auth_status().await?;
            let body = if status.detail.is_empty() {
                "Codex login completed.".to_string()
            } else {
                format!("Codex login completed.\n\n{}", status.detail)
            };
            send_markdown_message(&shared.telegram, chat_id, thread_id, &body, None).await?;
        }
        Err(error) => {
            if cancel.is_cancelled() || error.to_string().contains("cancelled") {
                return Ok(());
            }
            let body = format!("Codex login failed.\n\n{error:#}");
            send_markdown_message(&shared.telegram, chat_id, thread_id, &body, None).await?;
        }
    }
    Ok(())
}

async fn clear_pending_login(shared: &Arc<AppShared>, login_id: &str) {
    let mut pending = shared.pending_codex_login.lock().await;
    if pending.as_ref().map(|entry| entry.id.as_str()) == Some(login_id) {
        *pending = None;
    }
}

fn format_device_auth_message(
    prompt: &crate::codex::CodexDeviceAuthPrompt,
    already_running: bool,
) -> String {
    let prefix = if already_running {
        "Codex login is already in progress."
    } else {
        "Codex login started in headless device-code mode."
    };
    format!(
        "{prefix}\n\n1. Open [auth.openai.com]({})\n2. Sign in with your OpenAI account\n3. Enter code `{}`\n\nThe code expires in about 15 minutes. I will post the result here when the login finishes.",
        prompt.verification_uri, prompt.user_code
    )
}

fn codex_login_required_message() -> String {
    "Codex is not logged in.\n\nUse `/login`, then open [auth.openai.com/codex/device](https://auth.openai.com/codex/device) and enter the one-time code from the bot.".to_string()
}

async fn handle_login_start_failure(
    shared: &Arc<AppShared>,
    message: &Message,
    error: anyhow::Error,
) -> Result<()> {
    maybe_start_login_backoff(shared, &error).await;
    let body = format_login_failure_message(&error, shared).await;
    send_markdown_message(
        &shared.telegram,
        message.chat.id,
        message.message_thread_id,
        &body,
        None,
    )
    .await?;
    Ok(())
}

async fn maybe_start_login_backoff(shared: &Arc<AppShared>, error: &anyhow::Error) {
    if is_login_rate_limited(error) {
        let until = Instant::now() + Duration::from_secs(CODEX_LOGIN_BACKOFF_SECONDS);
        *shared.codex_login_backoff_until.lock().await = Some(until);
    }
}

async fn active_login_backoff_message(shared: &Arc<AppShared>) -> Option<String> {
    let mut guard = shared.codex_login_backoff_until.lock().await;
    let until = match *guard {
        Some(until) => until,
        None => return None,
    };
    if until <= Instant::now() {
        *guard = None;
        return None;
    }
    let seconds = until.duration_since(Instant::now()).as_secs().max(1);
    Some(format!(
        "Codex login is temporarily rate limited.\n\nWait about {seconds}s, then try `/login` again."
    ))
}

fn is_login_rate_limited(error: &anyhow::Error) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    text.contains("429 too many requests") || text.contains("rate limit")
}

async fn format_login_failure_message(error: &anyhow::Error, shared: &Arc<AppShared>) -> String {
    if is_login_rate_limited(error) {
        if let Some(backoff) = active_login_backoff_message(shared).await {
            return format!(
                "Codex login could not start because the device-code endpoint is rate limited.\n\n{backoff}"
            );
        }
    }
    format!("Codex login failed to start.\n\n{error:#}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_device_login_rate_limit_errors() {
        assert!(is_login_rate_limited(&anyhow!(
            "device code request failed with status 429 Too Many Requests"
        )));
        assert!(!is_login_rate_limited(&anyhow!(
            "timed out waiting for Codex device code"
        )));
    }
}
