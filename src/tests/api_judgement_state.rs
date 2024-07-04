use super::*;
use crate::api::VerifyChallenge;
use crate::api::{JsonResult, ResponseAccountState};
use crate::connector::WatcherMessage;
use crate::base::{
    ExpectedMessage, ExternalMessage, ExternalMessageType, IdentityContext, MessageId,
    NotificationMessage, Timestamp,
};
use actix_http::StatusCode;
use futures::{FutureExt, StreamExt};

#[actix::test]
async fn current_judgement_state_single_identity() {
    let (_db, connector, mut api, _inj) = new_env().await;
    let mut stream = api.ws_at("/api/account_status").await.unwrap();

    // Insert judgement request.
    connector.inject(alice_judgement_request()).await;
    let states = connector.inserted_states().await;
    let alice = states[0].clone();

    // Subscribe to endpoint.
    let resp = subscribe_context(&mut stream, IdentityContext::alice()).await;

    // Check current state.
    assert_eq!(
        resp,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(alice))
    );

    // Empty stream.
    assert!(stream.next().now_or_never().is_none());
}

#[actix::test]
async fn current_judgement_state_multiple_inserts() {
    let (_db, connector, mut api, _) = new_env().await;
    let mut stream = api.ws_at("/api/account_status").await.unwrap();

    // Insert judgement request.
    // Multiple inserts of the same request. Must not cause bad behavior.
    connector.inject(alice_judgement_request()).await;
    let states = connector.inserted_states().await;
    let alice = states[0].clone();

    connector.inject(alice_judgement_request()).await;

    // Subscribe to endpoint.
    let resp = subscribe_context(&mut stream, IdentityContext::alice()).await;

    // Check current state.
    assert_eq!(
        resp,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(alice))
    );

    // Empty stream.
    assert!(stream.next().now_or_never().is_none());
}

#[actix::test]
async fn current_judgement_state_multiple_identities() {
    let (_db, connector, mut api, _) = new_env().await;
    let mut stream = api.ws_at("/api/account_status").await.unwrap();

    // Insert judgement request.
    connector.inject(alice_judgement_request()).await;
    connector.inject(bob_judgement_request()).await;
    let states = connector.inserted_states().await;
    let alice = states[0].clone();
    let bob = states[1].clone();

    // Subscribe to endpoint.
    let resp = subscribe_context(&mut stream, IdentityContext::alice()).await;

    // Check current state (Alice).
    assert_eq!(
        resp,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(alice))
    );

    let resp = subscribe_context(&mut stream, IdentityContext::bob()).await;

    // Check current state (Bob).
    assert_eq!(
        resp,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(bob))
    );

    // Empty stream.
    assert!(stream.next().now_or_never().is_none());
}

#[actix::test]
async fn current_judgement_state_field_updated() {
    let (_db, connector, mut api, _) = new_env().await;
    let mut stream = api.ws_at("/api/account_status").await.unwrap();

    // Insert judgement request.
    connector.inject(alice_judgement_request()).await;
    connector.inject(bob_judgement_request()).await;
    let states = connector.inserted_states().await;
    let alice = states[0].clone();

    // Subscribe to endpoint.
    let resp = subscribe_context(&mut stream, IdentityContext::alice()).await;

    // Check current state (Alice).
    assert_eq!(
        resp,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(alice.clone()))
    );

    // New request with modified entries.
    let mut request = JudgementRequest::alice();
    request
        .accounts
        .entry(AccountType::Email)
        .and_modify(|entry| *entry = "alice_second@email.com".to_string());
    request.accounts.remove(&AccountType::Matrix);

    // Send request
    connector
        .inject(WatcherMessage::new_judgement_request(request))
        .await;

    // The expected message (identity updated).
    let resp: JsonResult<ResponseAccountState> = stream.next().await.into();

    match resp {
        JsonResult::Ok(resp) => {
            assert!(resp.state.fields.iter().any(|field| {
                field.value == IdentityFieldValue::Email("alice_second@email.com".to_string())
            }));
            assert!(!resp
                .state
                .fields
                .iter()
                .any(|field| { field.value == F::ALICE_MATRIX() }));

            assert_eq!(resp.notifications.len(), 1);
            assert_eq!(
                resp.notifications[0],
                NotificationMessage::IdentityUpdated {
                    context: alice.context.clone()
                }
            );
        }
        _ => panic!(),
    }

    // Empty stream.
    assert!(stream.next().now_or_never().is_none());
}

#[actix::test]
async fn current_judgement_state_single_entry_removed() {
    let (_db, connector, mut api, _) = new_env().await;
    let mut stream = api.ws_at("/api/account_status").await.unwrap();

    // Insert judgement request.
    connector.inject(alice_judgement_request()).await;
    connector.inject(bob_judgement_request()).await;
    let states = connector.inserted_states().await;
    let alice = states[0].clone();

    // Subscribe to endpoint.
    let resp = subscribe_context(&mut stream, IdentityContext::alice()).await;

    // Check current state (Alice).
    assert_eq!(
        resp,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(alice.clone()))
    );

    // New request with single entry removed.
    let mut request = JudgementRequest::alice();
    request.accounts.remove(&AccountType::Matrix);

    // Send request
    connector
        .inject(WatcherMessage::new_judgement_request(request))
        .await;

    // The expected message (identity updated).
    let resp: JsonResult<ResponseAccountState> = stream.next().await.into();

    match resp {
        JsonResult::Ok(resp) => {
            assert!(!resp
                .state
                .fields
                .iter()
                .any(|field| { field.value == F::ALICE_MATRIX() }));

            assert_eq!(resp.notifications.len(), 1);
            assert_eq!(
                resp.notifications[0],
                NotificationMessage::IdentityUpdated {
                    context: alice.context.clone()
                }
            );
        }
        _ => panic!(),
    }

    // Empty stream.
    assert!(stream.next().now_or_never().is_none());
}

#[actix::test]
async fn verify_invalid_message_bad_challenge() {
    let (_db, connector, mut api, injector) = new_env().await;
    let mut stream = api.ws_at("/api/account_status").await.unwrap();

    // Insert judgement requests.
    connector.inject(alice_judgement_request()).await;
    connector.inject(bob_judgement_request()).await;
    let states = connector.inserted_states().await;
    let mut alice = states[0].clone();
    let bob = states[1].clone();

    // Subscribe to endpoint.
    let resp = subscribe_context(&mut stream, IdentityContext::alice()).await;

    // Check current state.
    assert_eq!(
        resp,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(alice.clone()))
    );

    // Send invalid message (bad challenge).
    injector
        .send(ExternalMessage {
            origin: ExternalMessageType::Email("alice@email.com".to_string()),
            id: MessageId::from(0u32),
            timestamp: Timestamp::now(),
            values: ExpectedMessage::random().to_message_parts(),
        })
        .await;

    // The expected message (field verification failed).
    *alice.get_field_mut(&F::ALICE_EMAIL()).failed_attempts_mut() = 1;

    let expected = ResponseAccountState {
        state: alice.clone().into(),
        notifications: vec![NotificationMessage::FieldVerificationFailed {
            context: alice.context.clone(),
            field: F::ALICE_EMAIL(),
        }],
    };

    // Check response
    let resp: JsonResult<ResponseAccountState> = stream.next().await.into();
    assert_eq!(resp, JsonResult::Ok(expected));

    // Other judgement states must be unaffected (Bob).
    let resp = subscribe_context(&mut stream, IdentityContext::bob()).await;

    assert_eq!(
        resp,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(bob.clone()))
    );

    // Empty stream.
    assert!(stream.next().now_or_never().is_none());
}

#[actix::test]
async fn verify_invalid_message_bad_origin() {
    let (_db, connector, mut api, injector) = new_env().await;
    let mut stream = api.ws_at("/api/account_status").await.unwrap();

    // Insert judgement request.
    connector.inject(alice_judgement_request()).await;
    connector.inject(bob_judgement_request()).await;
    let states = connector.inserted_states().await;
    let alice = states[0].clone();
    let bob = states[1].clone();

    // Subscribe to endpoint.
    let resp = subscribe_context(&mut stream, IdentityContext::alice()).await;

    // Check current state.
    assert_eq!(
        resp,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(alice.clone()))
    );

    // Send invalid message (bad origin).
    injector
        .send(ExternalMessage {
            origin: ExternalMessageType::Email("eve@email.com".to_string()),
            id: MessageId::from(0u32),
            timestamp: Timestamp::now(),
            values: alice
                .get_field(&F::ALICE_EMAIL())
                .expected_message()
                .to_message_parts(),
        })
        .await;

    // No response is sent. The service ignores unknown senders.
    assert!(stream.next().now_or_never().is_none());

    // Other judgement states must be unaffected (Bob).
    let resp = subscribe_context(&mut stream, IdentityContext::bob()).await;

    assert_eq!(
        resp,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(bob.clone()))
    );

    // Empty stream.
    assert!(stream.next().now_or_never().is_none());
}

#[actix::test]
async fn verify_valid_message() {
    let (_db, connector, mut api, injector) = new_env().await;
    let mut stream = api.ws_at("/api/account_status").await.unwrap();

    // Insert judgement requests.
    connector.inject(alice_judgement_request()).await;
    connector.inject(bob_judgement_request()).await;
    let states = connector.inserted_states().await;
    let mut alice = states[0].clone();
    let bob = states[1].clone();

    // Subscribe to endpoint.
    let resp = subscribe_context(&mut stream, IdentityContext::alice()).await;

    // Check current state.
    assert_eq!(
        resp,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(alice.clone()))
    );

    // Send valid message.
    injector
        .send(ExternalMessage {
            origin: ExternalMessageType::Matrix("@alice:matrix.org".to_string()),
            id: MessageId::from(0u32),
            timestamp: Timestamp::now(),
            values: alice
                .get_field(&F::ALICE_MATRIX())
                .expected_message()
                .to_message_parts(),
        })
        .await;

    // Matrix account of Alice is now verified
    alice
        .get_field_mut(&F::ALICE_MATRIX())
        .expected_message_mut()
        .set_verified();

    // The expected message (field verified successfully).
    let expected = ResponseAccountState {
        state: alice.clone().into(),
        notifications: vec![NotificationMessage::FieldVerified {
            context: alice.context.clone(),
            field: F::ALICE_MATRIX(),
        }],
    };

    // Check response
    let resp: JsonResult<ResponseAccountState> = stream.next().await.into();
    assert_eq!(resp, JsonResult::Ok(expected));

    // Other judgement states must be unaffected (Bob).
    let resp = subscribe_context(&mut stream, IdentityContext::bob()).await;

    assert_eq!(
        resp,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(bob.clone()))
    );

    // Empty stream.
    assert!(stream.next().now_or_never().is_none());
}

#[actix::test]
async fn verify_valid_message_duplicate_account_name() {
    let (_db, connector, mut api, injector) = new_env().await;
    let mut stream = api.ws_at("/api/account_status").await.unwrap();

    // Insert judgement requests.
    connector.inject(alice_judgement_request()).await;
    // Note: Bob specified the same Matrix handle as Alice.
    connector
        .inject(WatcherMessage::new_judgement_request({
            let mut req = JudgementRequest::bob();
            req.accounts
                .entry(AccountType::Matrix)
                .and_modify(|e| *e = "@alice:matrix.org".to_string());
            req
        }))
        .await;

    let states = connector.inserted_states().await;
    let mut alice = states[0].clone();
    let mut bob = states[1].clone();

    // Subscribe to endpoint.
    let resp = subscribe_context(&mut stream, IdentityContext::alice()).await;

    // Check current state.
    assert_eq!(
        resp,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(alice.clone()))
    );

    // Send valid message.
    injector
        .send(ExternalMessage {
            origin: ExternalMessageType::Matrix("@alice:matrix.org".to_string()),
            id: MessageId::from(0u32),
            timestamp: Timestamp::now(),
            values: alice
                .get_field(&F::ALICE_MATRIX())
                .expected_message()
                .to_message_parts(),
        })
        .await;

    // Email account of Alice is now verified
    alice
        .get_field_mut(&F::ALICE_MATRIX())
        .expected_message_mut()
        .set_verified();

    // The expected message (field verified successfully).
    let expected = ResponseAccountState {
        state: alice.clone().into(),
        notifications: vec![NotificationMessage::FieldVerified {
            context: alice.context.clone(),
            field: F::ALICE_MATRIX(),
        }],
    };

    // Check response
    let resp: JsonResult<ResponseAccountState> = stream.next().await.into();
    assert_eq!(resp, JsonResult::Ok(expected));

    // Other judgement states must be unaffected (Bob), but will receive a "failed attempt".
    let resp = subscribe_context(&mut stream, IdentityContext::bob()).await;

    *bob.get_field_mut(&F::ALICE_MATRIX()).failed_attempts_mut() = 1;

    assert_eq!(
        resp,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(bob.clone()))
    );

    // Empty stream.
    assert!(stream.next().now_or_never().is_none());
}

#[actix::test]
async fn verify_valid_message_awaiting_second_challenge() {
    let (_db, connector, mut api, injector) = new_env().await;
    let mut stream = api.ws_at("/api/account_status").await.unwrap();

    // Insert judgement requests.
    connector.inject(alice_judgement_request()).await;
    connector.inject(bob_judgement_request()).await;
    let states = connector.inserted_states().await;
    let mut alice = states[0].clone();
    let bob = states[1].clone();

    // Subscribe to endpoint.
    let resp = subscribe_context(&mut stream, IdentityContext::alice()).await;

    // Check current state.
    assert_eq!(
        resp,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(alice.clone()))
    );

    // Send valid message.
    injector
        .send(ExternalMessage {
            origin: ExternalMessageType::Email("alice@email.com".to_string()),
            id: MessageId::from(0u32),
            timestamp: Timestamp::now(),
            values: alice
                .get_field(&F::ALICE_EMAIL())
                .expected_message()
                .to_message_parts(),
        })
        .await;

    // Email account of Alice is now verified, but not the second challenge.
    alice
        .get_field_mut(&F::ALICE_EMAIL())
        .expected_message_mut()
        .set_verified();

    // Check for `FieldVerified` notification.
    let expected = ResponseAccountState {
        state: alice.clone().into(),
        notifications: vec![NotificationMessage::FieldVerified {
            context: alice.context.clone(),
            field: F::ALICE_EMAIL(),
        }],
    };

    // Check response.
    let resp: JsonResult<ResponseAccountState> = stream.next().await.into();
    assert_eq!(resp, JsonResult::Ok(expected));

    // Check for `AwaitingSecondChallenge` notification.
    let expected = ResponseAccountState {
        state: alice.clone().into(),
        notifications: vec![NotificationMessage::AwaitingSecondChallenge {
            context: alice.context.clone(),
            field: F::ALICE_EMAIL(),
        }],
    };

    let resp: JsonResult<ResponseAccountState> = stream.next().await.into();
    assert_eq!(resp, JsonResult::Ok(expected));

    // Verify second challenge.
    let challenge = VerifyChallenge {
        entry: F::ALICE_EMAIL(),
        challenge: alice
            .get_field(&F::ALICE_EMAIL())
            .expected_second()
            .value
            .clone(),
    };

    // Send it to the API endpoint.
    let res = api
        .post("/api/verify_second_challenge")
        .send_json(&challenge)
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);

    // Second challenge of email account of Alice is now verified
    alice
        .get_field_mut(&F::ALICE_EMAIL())
        .expected_second_mut()
        .set_verified();

    // Check for `SecondFieldVerified` notification.
    let expected = ResponseAccountState {
        state: alice.clone().into(),
        notifications: vec![NotificationMessage::SecondFieldVerified {
            context: alice.context.clone(),
            field: F::ALICE_EMAIL(),
        }],
    };

    let resp: JsonResult<ResponseAccountState> = stream.next().await.into();
    assert_eq!(resp, JsonResult::Ok(expected));

    // Other judgement state must be unaffected (Bob).
    let resp = subscribe_context(&mut stream, IdentityContext::bob()).await;

    assert_eq!(
        resp,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(bob.clone()))
    );

    // Empty stream.
    assert!(stream.next().now_or_never().is_none());
}

#[actix::test]
async fn verify_invalid_message_awaiting_second_challenge() {
    let (_db, connector, mut api, injector) = new_env().await;
    let mut stream = api.ws_at("/api/account_status").await.unwrap();

    // Insert judgement requests.
    connector.inject(alice_judgement_request()).await;
    connector.inject(bob_judgement_request()).await;
    let states = connector.inserted_states().await;
    let mut alice = states[0].clone();
    let bob = states[1].clone();

    // Subscribe to endpoint.
    let resp = subscribe_context(&mut stream, IdentityContext::alice()).await;

    // Check current state.
    assert_eq!(
        resp,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(alice.clone()))
    );

    // Send valid message.
    injector
        .send(ExternalMessage {
            origin: ExternalMessageType::Email("alice@email.com".to_string()),
            id: MessageId::from(0u32),
            timestamp: Timestamp::now(),
            values: alice
                .get_field(&F::ALICE_EMAIL())
                .expected_message()
                .to_message_parts(),
        })
        .await;

    // Email account of Alice is now verified, but not the second challenge.
    alice
        .get_field_mut(&F::ALICE_EMAIL())
        .expected_message_mut()
        .set_verified();

    // Check for `FieldVerified` notification.
    let expected = ResponseAccountState {
        state: alice.clone().into(),
        notifications: vec![NotificationMessage::FieldVerified {
            context: alice.context.clone(),
            field: F::ALICE_EMAIL(),
        }],
    };

    // Check response.
    let resp: JsonResult<ResponseAccountState> = stream.next().await.into();
    assert_eq!(resp, JsonResult::Ok(expected));

    // Check for `AwaitingSecondChallenge` notification.
    let expected = ResponseAccountState {
        state: alice.clone().into(),
        notifications: vec![NotificationMessage::AwaitingSecondChallenge {
            context: alice.context.clone(),
            field: F::ALICE_EMAIL(),
        }],
    };

    let resp: JsonResult<ResponseAccountState> = stream.next().await.into();
    assert_eq!(resp, JsonResult::Ok(expected));

    // Send invalid second challenge.
    let challenge = VerifyChallenge {
        entry: F::ALICE_EMAIL(),
        challenge: "INVALID".to_string(),
    };

    // Send it to the API endpoint.
    let res = api
        .post("/api/verify_second_challenge")
        .send_json(&challenge)
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);

    // Check for `SecondFieldVerified` notification.
    let expected = ResponseAccountState {
        state: alice.clone().into(),
        notifications: vec![NotificationMessage::SecondFieldVerificationFailed {
            context: alice.context.clone(),
            field: F::ALICE_EMAIL(),
        }],
    };

    let resp: JsonResult<ResponseAccountState> = stream.next().await.into();
    assert_eq!(resp, JsonResult::Ok(expected));

    // Other judgement state must be unaffected (Bob).
    let resp = subscribe_context(&mut stream, IdentityContext::bob()).await;

    assert_eq!(
        resp,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(bob.clone()))
    );

    // Empty stream.
    assert!(stream.next().now_or_never().is_none());
}

#[actix::test]
async fn verify_full_identity() {
    let (db, connector, mut api, _injector) = new_env().await;
    let mut stream_alice = api.ws_at("/api/account_status").await.unwrap();
    let mut stream_bob = api.ws_at("/api/account_status").await.unwrap();

    // Insert judgement requests.
    connector.inject(alice_judgement_request()).await;
    connector.inject(bob_judgement_request()).await;
    let states = connector.inserted_states().await;
    let mut alice = states[0].clone();
    let bob = states[1].clone();

    // Subscribe to endpoint.
    let resp_alice = subscribe_context(&mut stream_alice, IdentityContext::alice()).await;
    let resp_bob = subscribe_context(&mut stream_bob, IdentityContext::bob()).await;

    // Check initial state
    assert_eq!(
        resp_alice,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(alice.clone()))
    );

    // Verify Display name (does not create notification).
    db.set_display_name_valid(&alice).await.unwrap();
    let passed = alice
        .get_field_mut(&F::ALICE_DISPLAY_NAME())
        .expected_display_name_check_mut()
        .0;
    *passed = true;

    // Check updated state with notification.
    let exp_resp = ResponseAccountState {
        state: alice.clone().into(),
        notifications: vec![NotificationMessage::FieldVerified {
            context: alice.context.clone(),
            field: F::ALICE_DISPLAY_NAME(),
        }],
    };

    let resp: JsonResult<ResponseAccountState> = stream_alice.next().await.into();
    assert_eq!(resp, JsonResult::Ok(exp_resp));

    // Verify Twitter.
    let msg = ExternalMessage {
        origin: ExternalMessageType::Twitter("@alice".to_string()),
        id: MessageId::from(0u32),
        timestamp: Timestamp::now(),
        values: alice
            .get_field(&F::ALICE_TWITTER())
            .expected_message()
            .to_message_parts(),
    };

    // Verify whether message is valid.
    let exp_message = alice
        .get_field_mut(&F::ALICE_TWITTER())
        .expected_message_mut();

    let changed = exp_message.is_message_valid(&msg);

    assert!(changed);

    // Set valid.
    exp_message.set_verified();
    db.verify_message(&msg).await.unwrap();

    // Check updated state with notification.
    let exp_resp = ResponseAccountState {
        state: alice.clone().into(),
        notifications: vec![NotificationMessage::FieldVerified {
            context: alice.context.clone(),
            field: F::ALICE_TWITTER(),
        }],
    };

    let resp: JsonResult<ResponseAccountState> = stream_alice.next().await.into();
    assert_eq!(resp, JsonResult::Ok(exp_resp));

    // Verify Email (first challenge).
    let msg = ExternalMessage {
        origin: ExternalMessageType::Email("alice@email.com".to_string()),
        id: MessageId::from(0u32),
        timestamp: Timestamp::now(),
        values: alice
            .get_field(&F::ALICE_EMAIL())
            .expected_message()
            .to_message_parts(),
    };

    let exp_message = alice
        .get_field_mut(&F::ALICE_EMAIL())
        .expected_message_mut();

    let changed = exp_message.is_message_valid(&msg);

    assert!(changed);

    // Set valid
    exp_message.set_verified();
    db.verify_message(&msg).await.unwrap();

    // Check updated state with notification.
    let exp_resp = ResponseAccountState {
        state: alice.clone().into(),
        notifications: vec![NotificationMessage::FieldVerified {
            context: alice.context.clone(),
            field: F::ALICE_EMAIL(),
        }],
    };

    let resp: JsonResult<ResponseAccountState> = stream_alice.next().await.into();
    assert_eq!(resp, JsonResult::Ok(exp_resp));

    let exp_resp = ResponseAccountState {
        state: alice.clone().into(),
        notifications: vec![NotificationMessage::AwaitingSecondChallenge {
            context: alice.context.clone(),
            field: F::ALICE_EMAIL(),
        }],
    };

    let resp: JsonResult<ResponseAccountState> = stream_alice.next().await.into();
    assert_eq!(resp, JsonResult::Ok(exp_resp));

    // Second challenge
    alice
        .get_field_mut(&F::ALICE_EMAIL())
        .expected_second_mut()
        .set_verified();

    db.verify_second_challenge(VerifyChallenge {
        entry: F::ALICE_EMAIL(),
        challenge: alice
            .get_field(&F::ALICE_EMAIL())
            .expected_second()
            .value
            .to_string(),
    })
    .await
    .unwrap();

    // Check updated state with notification.
    let exp_resp = ResponseAccountState {
        state: alice.clone().into(),
        notifications: vec![NotificationMessage::SecondFieldVerified {
            context: alice.context.clone(),
            field: F::ALICE_EMAIL(),
        }],
    };

    let resp: JsonResult<ResponseAccountState> = stream_alice.next().await.into();
    assert_eq!(resp, JsonResult::Ok(exp_resp));

    // Verify Matrix.
    let msg = ExternalMessage {
        origin: ExternalMessageType::Matrix("@alice:matrix.org".to_string()),
        id: MessageId::from(0u32),
        timestamp: Timestamp::now(),
        values: alice
            .get_field(&F::ALICE_MATRIX())
            .expected_message()
            .to_message_parts(),
    };

    let exp_message = alice
        .get_field_mut(&F::ALICE_MATRIX())
        .expected_message_mut();

    let changed = exp_message.is_message_valid(&msg);

    assert!(changed);

    // Set valid
    exp_message.set_verified();
    db.verify_message(&msg).await.unwrap();

    // Check updated state with notification.
    // Identity is fully verified now.

    let resp: JsonResult<ResponseAccountState> = stream_alice.next().await.into();
    // The completion timestamp is not that important, as long as it's `Some`.
    let completion_timestamp = match &resp {
        JsonResult::Ok(r) => r.state.completion_timestamp,
        _ => panic!(),
    };

    assert!(completion_timestamp.is_some());
    alice.is_fully_verified = true;
    alice.completion_timestamp = completion_timestamp;

    let exp_resp = ResponseAccountState {
        state: alice.clone().into(),
        notifications: vec![NotificationMessage::FieldVerified {
            context: alice.context.clone(),
            field: F::ALICE_MATRIX(),
        }],
    };

    assert_eq!(resp, JsonResult::Ok(exp_resp));

    let resp: JsonResult<ResponseAccountState> = stream_alice.next().await.into();
    let exp_resp = ResponseAccountState {
        state: alice.clone().into(),
        notifications: vec![NotificationMessage::IdentityFullyVerified {
            context: alice.context.clone(),
        }],
    };

    assert_eq!(resp, JsonResult::Ok(exp_resp));

    // Bob remains unchanged.
    assert_eq!(
        resp_bob,
        JsonResult::Ok(ResponseAccountState::with_no_notifications(bob.clone()))
    );

    // Empty stream.
    assert!(stream_alice.next().now_or_never().is_none());
}
