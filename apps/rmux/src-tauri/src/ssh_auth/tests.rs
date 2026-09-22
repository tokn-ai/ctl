use super::*;
use tokio::sync::mpsc;
use tokio::time::timeout;

fn prompt_context() -> (PromptContext, mpsc::UnboundedReceiver<serde_json::Value>) {
  let (sender, receiver) = mpsc::unbounded_channel();
  let channel = Channel::new(move |body| {
    let _ = sender.send(body.deserialize::<serde_json::Value>().unwrap());
    Ok(())
  });
  let (cancel, _) = watch::channel(false);
  (
    PromptContext {
      attempt: Arc::new(Attempt {
        target: ctld_ipc::SshTarget {
          destination: "test".into(),
          ssh_config_alias: None,
          use_ssh_config_master: None,
          hostname: None,
          user: None,
          port: None,
          identity_file: None,
          gateways: Vec::new(),
        },
        cancel,
        responses: Mutex::default(),
      }),
      channel,
    },
    receiver,
  )
}

#[tokio::test]
async fn prompt_responses_are_window_scoped_single_use_and_cancellable() {
  let (context, mut prompts) = prompt_context();
  let key = ("test-window".into(), uuid::Uuid::new_v4().to_string());
  registry()
    .lock()
    .unwrap()
    .attempts
    .insert(key.clone(), context.attempt.clone());
  let guard = AttemptGuard(key.clone());
  let mut cancelled = context.attempt.cancel.subscribe();
  let task = tokio::spawn(async move {
    request_response(Some(&context), SshPromptKind::Secret, "Password:".into()).await
  });
  let prompt = timeout(Duration::from_secs(5), prompts.recv())
    .await
    .unwrap()
    .unwrap();
  let prompt_id = prompt["prompt_id"].as_str().unwrap();
  assert_eq!(prompt["kind"], "secret");
  assert!(
    respond(
      "other-window",
      &key.1,
      prompt_id,
      Some("synthetic-secret".into())
    )
    .is_err()
  );
  assert!(respond(&key.0, &key.1, prompt_id, Some("invalid\nresponse".into())).is_err());
  respond(&key.0, &key.1, prompt_id, Some("synthetic-secret".into())).unwrap();
  assert!(respond(&key.0, &key.1, prompt_id, None).is_err());
  assert_eq!(task.await.unwrap().unwrap().as_str(), "synthetic-secret");

  cancel_window(&key.0);
  cancelled.changed().await.unwrap();
  assert!(*cancelled.borrow());
  drop(guard);
  assert!(!registry().lock().unwrap().attempts.contains_key(&key));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn credential_save_choices_use_the_prompt_channel_without_sending_a_secret() {
  let (context, mut prompts) = prompt_context();
  let attempt = context.attempt.clone();
  let task = tokio::spawn(async move {
    request_response(
      Some(&context),
      SshPromptKind::CredentialSave,
      "Save this SSH credential?".into(),
    )
    .await
  });
  let prompt = timeout(Duration::from_secs(5), prompts.recv())
    .await
    .unwrap()
    .unwrap();
  assert_eq!(prompt["kind"], "credential_save");
  assert_eq!(prompt["message"], "Save this SSH credential?");
  attempt
    .responses
    .lock()
    .unwrap()
    .remove(prompt["prompt_id"].as_str().unwrap())
    .unwrap()
    .send(Some(Zeroizing::new("never".into())))
    .unwrap();
  assert_eq!(task.await.unwrap().unwrap().as_str(), "never");
}
