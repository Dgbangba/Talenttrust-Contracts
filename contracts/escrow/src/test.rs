extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events, MockAuth, MockAuthInvoke},
    token::StellarAssetClient,
    token::TokenClient,
    vec, IntoVal, Symbol, TryFromVal,
};

struct Setup<'a> {
    env: Env,
    escrow: EscrowClient<'a>,
    token: TokenClient<'a>,
    client: Address,
    freelancer: Address,
}

fn setup<'a>() -> Setup<'a> {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(admin.clone());
    StellarAssetClient::new(&env, &sac.address()).mint(&client, &1_000);
    let id = env.register(Escrow, ());
    let escrow = EscrowClient::new(&env, &id);
    escrow.initialize(&admin);
    let token = TokenClient::new(&env, &sac.address());
    Setup {
        env,
        escrow,
        token,
        client,
        freelancer,
    }
}

#[test]
fn initialize_only_once() {
    let s = setup();
    let other = Address::generate(&s.env);
    assert_eq!(
        s.escrow.try_initialize(&other),
        Err(Ok(EscrowError::AlreadyInitialized))
    );
}

#[test]
fn full_happy_path() {
    let s = setup();
    let id = s.escrow.create_contract(
        &s.client,
        &s.freelancer,
        &s.token.address,
        &vec![&s.env, 100i128, 200i128],
    );
    assert_eq!(id, 1);
    s.escrow.deposit_funds(&id, &300);
    assert_eq!(s.token.balance(&s.client), 700);
    assert_eq!(s.escrow.get_contract(&id).status, ContractStatus::Funded);
    s.escrow.release_milestone(&id, &0);
    s.escrow.release_milestone(&id, &1);
    assert_eq!(s.token.balance(&s.freelancer), 300);
    assert_eq!(s.escrow.get_contract(&id).status, ContractStatus::Completed);
    s.escrow.issue_reputation(&id, &5);
    let rep = s.escrow.get_reputation(&s.freelancer);
    assert_eq!(rep.completed_contracts, 1);
    assert_eq!(rep.total_rating, 5);
}

#[test]
fn rejects_overfunding_and_double_release() {
    let s = setup();
    let id = s.escrow.create_contract(
        &s.client,
        &s.freelancer,
        &s.token.address,
        &vec![&s.env, 100i128],
    );
    assert_eq!(
        s.escrow.try_deposit_funds(&id, &101),
        Err(Ok(EscrowError::OverFunded))
    );
    s.escrow.deposit_funds(&id, &100);
    s.escrow.release_milestone(&id, &0);
    assert!(s.escrow.try_release_milestone(&id, &0).is_err());
}

#[test]
fn dispute_split_and_cancel() {
    let s = setup();
    let id = s.escrow.create_contract(
        &s.client,
        &s.freelancer,
        &s.token.address,
        &vec![&s.env, 400i128],
    );
    s.escrow.deposit_funds(&id, &400);
    s.escrow.open_dispute(&id, &s.freelancer);
    s.escrow.resolve_dispute(&id, &2_500);
    assert_eq!(s.token.balance(&s.freelancer), 100);
    assert_eq!(s.token.balance(&s.client), 900);

    let id2 = s.escrow.create_contract(
        &s.client,
        &s.freelancer,
        &s.token.address,
        &vec![&s.env, 50i128],
    );
    s.escrow.deposit_funds(&id2, &50);
    s.escrow.cancel_contract(&id2);
    assert_eq!(s.token.balance(&s.client), 900);
    assert_eq!(
        s.escrow.get_contract(&id2).status,
        ContractStatus::Cancelled
    );
}

#[test]
fn pause_blocks_mutations() {
    let s = setup();
    s.escrow.set_paused(&true);
    assert_eq!(
        s.escrow.try_create_contract(
            &s.client,
            &s.freelancer,
            &s.token.address,
            &vec![&s.env, 1i128]
        ),
        Err(Ok(EscrowError::Paused))
    );
}

#[test]
fn test_rate_client_completed_success_and_event() {
    let s = setup();
    let id = s.escrow.create_contract(
        &s.client,
        &s.freelancer,
        &s.token.address,
        &vec![&s.env, 150i128],
    );
    s.escrow.deposit_funds(&id, &150);
    s.escrow.release_milestone(&id, &0);
    assert_eq!(s.escrow.get_contract(&id).status, ContractStatus::Completed);

    // Initial reputation of client is default (0 completed, 0 rating)
    assert_eq!(
        s.escrow.get_client_reputation(&s.client),
        Reputation {
            completed_contracts: 0,
            total_rating: 0,
        }
    );

    // Freelancer rates client with rating 5
    s.escrow.rate_client(&id, &5);

    // Verify event topics: ("rep_cli", id) and data: (client, rating)
    let events = s.env.events().all();
    let last = events.last().unwrap();
    assert_eq!(last.0, s.escrow.address);
    assert_eq!(last.1.len(), 2);
    let topic_sym = Symbol::try_from_val(&s.env, &last.1.get(0).unwrap()).unwrap();
    let topic_id = u64::try_from_val(&s.env, &last.1.get(1).unwrap()).unwrap();
    assert_eq!(topic_sym, symbol_short!("rep_cli"));
    assert_eq!(topic_id, id);

    let (event_client, event_rating) = <(Address, u32)>::try_from_val(&s.env, &last.2).unwrap();
    assert_eq!(event_client, s.client);
    assert_eq!(event_rating, 5u32);

    // Reputation updated under client address
    let rep = s.escrow.get_client_reputation(&s.client);
    assert_eq!(
        rep,
        Reputation {
            completed_contracts: 1,
            total_rating: 5,
        }
    );

    // Unrelated addresses have default empty reputation
    let other = Address::generate(&s.env);
    assert_eq!(
        s.escrow.get_client_reputation(&other),
        Reputation::default()
    );

    // Freelancer's reputation is not touched by client rating
    assert_eq!(
        s.escrow.get_reputation(&s.freelancer),
        Reputation::default()
    );
}

#[test]
fn test_rate_client_resolved_success() {
    let s = setup();
    let id = s.escrow.create_contract(
        &s.client,
        &s.freelancer,
        &s.token.address,
        &vec![&s.env, 200i128],
    );
    s.escrow.deposit_funds(&id, &200);
    s.escrow.open_dispute(&id, &s.freelancer);
    s.escrow.resolve_dispute(&id, &5_000);
    assert_eq!(s.escrow.get_contract(&id).status, ContractStatus::Resolved);

    // Freelancer rates client after dispute resolution
    s.escrow.rate_client(&id, &4);

    let rep = s.escrow.get_client_reputation(&s.client);
    assert_eq!(
        rep,
        Reputation {
            completed_contracts: 1,
            total_rating: 4,
        }
    );
}

#[test]
fn test_rate_client_authorization() {
    let s = setup();
    let id = s.escrow.create_contract(
        &s.client,
        &s.freelancer,
        &s.token.address,
        &vec![&s.env, 100i128],
    );
    s.escrow.deposit_funds(&id, &100);
    s.escrow.release_milestone(&id, &0);
    assert_eq!(s.escrow.get_contract(&id).status, ContractStatus::Completed);

    // Client attempting to call rate_client is rejected
    let res = s
        .escrow
        .mock_auths(&[MockAuth {
            address: &s.client,
            invoke: &MockAuthInvoke {
                contract: &s.escrow.address,
                fn_name: "rate_client",
                args: (id, 5u32).into_val(&s.env),
                sub_invokes: &[],
            },
        }])
        .try_rate_client(&id, &5);
    assert!(res.is_err());

    // Arbitrary third party calling rate_client is rejected
    let attacker = Address::generate(&s.env);
    let res_attacker = s
        .escrow
        .mock_auths(&[MockAuth {
            address: &attacker,
            invoke: &MockAuthInvoke {
                contract: &s.escrow.address,
                fn_name: "rate_client",
                args: (id, 5u32).into_val(&s.env),
                sub_invokes: &[],
            },
        }])
        .try_rate_client(&id, &5);
    assert!(res_attacker.is_err());

    // Freelancer calling rate_client succeeds
    let res_ok = s
        .escrow
        .mock_auths(&[MockAuth {
            address: &s.freelancer,
            invoke: &MockAuthInvoke {
                contract: &s.escrow.address,
                fn_name: "rate_client",
                args: (id, 5u32).into_val(&s.env),
                sub_invokes: &[],
            },
        }])
        .try_rate_client(&id, &5);
    assert_eq!(res_ok, Ok(Ok(())));
}

#[test]
fn test_rate_client_duplicate_rejection() {
    let s = setup();
    let id = s.escrow.create_contract(
        &s.client,
        &s.freelancer,
        &s.token.address,
        &vec![&s.env, 100i128],
    );
    s.escrow.deposit_funds(&id, &100);
    s.escrow.release_milestone(&id, &0);

    // First rating succeeds
    s.escrow.rate_client(&id, &5);

    // Second rating for the same escrow receives ReputationAlreadyIssued
    assert_eq!(
        s.escrow.try_rate_client(&id, &4),
        Err(Ok(EscrowError::ReputationAlreadyIssued))
    );
}

#[test]
fn test_rate_client_validation() {
    let s = setup();
    let id = s.escrow.create_contract(
        &s.client,
        &s.freelancer,
        &s.token.address,
        &vec![&s.env, 100i128],
    );
    s.escrow.deposit_funds(&id, &100);
    s.escrow.release_milestone(&id, &0);

    // Rating 0 is rejected
    assert_eq!(
        s.escrow.try_rate_client(&id, &0),
        Err(Ok(EscrowError::InvalidRating))
    );

    // Rating 6 is rejected
    assert_eq!(
        s.escrow.try_rate_client(&id, &6),
        Err(Ok(EscrowError::InvalidRating))
    );

    // Valid rating 1..=5 accepted
    assert_eq!(s.escrow.try_rate_client(&id, &3), Ok(Ok(())));
    let rep = s.escrow.get_client_reputation(&s.client);
    assert_eq!(rep.completed_contracts, 1);
    assert_eq!(rep.total_rating, 3);
}

#[test]
fn test_rate_client_status_rejections() {
    let s = setup();

    // 1. Escrow in Funded status rejected
    let id_funded = s.escrow.create_contract(
        &s.client,
        &s.freelancer,
        &s.token.address,
        &vec![&s.env, 100i128],
    );
    s.escrow.deposit_funds(&id_funded, &100);
    assert_eq!(
        s.escrow.get_contract(&id_funded).status,
        ContractStatus::Funded
    );
    assert_eq!(
        s.escrow.try_rate_client(&id_funded, &5),
        Err(Ok(EscrowError::InvalidStatus))
    );

    // 2. Escrow in Cancelled status rejected
    let id_cancelled = s.escrow.create_contract(
        &s.client,
        &s.freelancer,
        &s.token.address,
        &vec![&s.env, 50i128],
    );
    s.escrow.deposit_funds(&id_cancelled, &50);
    s.escrow.cancel_contract(&id_cancelled);
    assert_eq!(
        s.escrow.get_contract(&id_cancelled).status,
        ContractStatus::Cancelled
    );
    assert_eq!(
        s.escrow.try_rate_client(&id_cancelled, &5),
        Err(Ok(EscrowError::InvalidStatus))
    );
}

#[test]
fn test_reputation_independence() {
    let s = setup();

    // Contract 1: Client issues reputation first, then freelancer rates client
    let id1 = s.escrow.create_contract(
        &s.client,
        &s.freelancer,
        &s.token.address,
        &vec![&s.env, 100i128],
    );
    s.escrow.deposit_funds(&id1, &100);
    s.escrow.release_milestone(&id1, &0);

    // Client rates freelancer
    s.escrow.issue_reputation(&id1, &5);
    let fl_rep1 = s.escrow.get_reputation(&s.freelancer);
    assert_eq!(fl_rep1.completed_contracts, 1);
    assert_eq!(fl_rep1.total_rating, 5);

    // Freelancer rates client afterwards; client-rating is not blocked
    s.escrow.rate_client(&id1, &4);
    let cl_rep1 = s.escrow.get_client_reputation(&s.client);
    assert_eq!(cl_rep1.completed_contracts, 1);
    assert_eq!(cl_rep1.total_rating, 4);

    // Contract 2: Freelancer rates client first, then client rates freelancer
    let id2 = s.escrow.create_contract(
        &s.client,
        &s.freelancer,
        &s.token.address,
        &vec![&s.env, 100i128],
    );
    s.escrow.deposit_funds(&id2, &100);
    s.escrow.release_milestone(&id2, &0);

    // Freelancer rates client first
    s.escrow.rate_client(&id2, &5);
    let cl_rep2 = s.escrow.get_client_reputation(&s.client);
    assert_eq!(cl_rep2.completed_contracts, 2);
    assert_eq!(cl_rep2.total_rating, 9);

    // Client rates freelancer afterwards; freelancer-rating is not blocked
    s.escrow.issue_reputation(&id2, &4);
    let fl_rep2 = s.escrow.get_reputation(&s.freelancer);
    assert_eq!(fl_rep2.completed_contracts, 2);
    assert_eq!(fl_rep2.total_rating, 9);

    // Duplicate client rating on id2 rejected
    assert_eq!(
        s.escrow.try_rate_client(&id2, &5),
        Err(Ok(EscrowError::ReputationAlreadyIssued))
    );
    // Duplicate freelancer rating on id2 rejected
    assert_eq!(
        s.escrow.try_issue_reputation(&id2, &4),
        Err(Ok(EscrowError::ReputationAlreadyIssued))
    );
}

#[test]
fn test_rate_client_pause_gate() {
    let s = setup();
    let id = s.escrow.create_contract(
        &s.client,
        &s.freelancer,
        &s.token.address,
        &vec![&s.env, 100i128],
    );
    s.escrow.deposit_funds(&id, &100);
    s.escrow.release_milestone(&id, &0);
    assert_eq!(s.escrow.get_contract(&id).status, ContractStatus::Completed);

    // Pause contract
    s.escrow.set_paused(&true);
    assert_eq!(
        s.escrow.try_rate_client(&id, &5),
        Err(Ok(EscrowError::Paused))
    );

    // Unpause contract and verify rating succeeds
    s.escrow.set_paused(&false);
    assert_eq!(s.escrow.try_rate_client(&id, &5), Ok(Ok(())));
    let rep = s.escrow.get_client_reputation(&s.client);
    assert_eq!(rep.completed_contracts, 1);
    assert_eq!(rep.total_rating, 5);
}
