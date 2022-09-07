use std::{str::FromStr, sync::Arc};

use anyhow::{Error, Result};
use async_trait::async_trait;
use hashbrown::HashMap;
use solana_account_decoder::UiAccountEncoding;
use solana_client::{
    rpc_client::RpcClient,
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig, RpcSendTransactionConfig},
    rpc_filter::RpcFilterType,
};
use solana_sdk::{
    account::Account,
    clock::Clock,
    commitment_config::CommitmentConfig,
    hash::Hash,
    instruction::Instruction,
    message::Message,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};
use solana_transaction_status::TransactionStatus;

use crate::solana_rpc_interface::{AsyncSigner, SolanaRpcInterface, TransactionContext};

pub struct Client {
    conn: Arc<RpcClient>,
    ctx: Context,
}

pub struct Context {
    endpoint: Network,
    kps: Keys,
    tx_config: Option<RpcSendTransactionConfig>,
}

impl Client {
    pub fn new(network_desc: &str) -> Result<Self> {
        let endpoint = Network::from_str(network_desc)?;
        let rpc = RpcClient::new(endpoint.to_string());
        let ctx = Context::new(endpoint)?;

        Ok(Self {
            conn: Arc::new(rpc),
            ctx,
        })
    }

    pub fn new_with_payer(network_desc: &str, payer: Keypair) -> Result<Self> {
        let endpoint = Network::from_str(network_desc)?;
        let rpc = RpcClient::new(endpoint.to_string());
        let mut ctx = Context::new(endpoint)?;
        ctx.kps.insert("payer", payer);

        Ok(Self {
            conn: Arc::new(rpc),
            ctx,
        })
    }
    pub fn new_with_config(
        network_desc: &str,
        tx_config: RpcSendTransactionConfig,
    ) -> Result<Self> {
        let endpoint = Network::from_str(network_desc)?;
        let rpc = RpcClient::new(endpoint.to_string());
        let mut ctx = Context::new(endpoint)?;
        ctx.tx_config = Some(tx_config);

        Ok(Self {
            conn: Arc::new(rpc),
            ctx,
        })
    }

    pub fn new_with_payer_and_config(
        network_desc: &str,
        payer: Keypair,
        tx_config: RpcSendTransactionConfig,
    ) -> Result<Self> {
        let mut client = Self::new_with_config(network_desc, tx_config)?;
        client.insert_keypair("payer", payer);

        Ok(client)
    }

    pub fn insert_keypair(&mut self, desc: &str, kp: Keypair) {
        self.ctx.kps.insert(desc, kp)
    }

    /// Optimistic = assume there is no risk. so we don't need:
    /// - finality (processed can be trusted)
    /// - preflight checks (not worried about losing sol)
    ///
    /// This is desirable for testing because:
    /// - tests can run faster (never need to wait for finality)
    /// - validator logs are more comprehensive (preflight checks obscure error logs)
    /// - there is nothing at stake in a local test validator
    pub fn new_optimistic(network_desc: &str) -> Result<Self> {
        let endpoint = Network::from_str(network_desc)?;
        let rpc =
            RpcClient::new_with_commitment(endpoint.to_string(), CommitmentConfig::processed());
        let ctx = Context::new_optimistic(endpoint)?;
        Ok(Self {
            conn: Arc::new(rpc),
            ctx,
        })
    }

    /// Optimistic client for a local validator with a funded payer
    pub async fn new_local_funded() -> Result<Self> {
        let mut client = Self::new_optimistic(&Network::Localnet.to_string())?;

        let payer = Keypair::new();
        client.conn.request_airdrop(
            &payer.pubkey(),
            100_000 * solana_sdk::native_token::LAMPORTS_PER_SOL,
        )?;

        client.insert_keypair("payer", payer);

        Ok(client)
    }

    pub fn endpoint(&self) -> Network {
        self.ctx.endpoint.clone()
    }
}

impl Context {
    pub fn new(endpoint: Network) -> Result<Self> {
        Ok(Self {
            endpoint,
            kps: Keys::new(),
            tx_config: None,
        })
    }

    pub fn new_optimistic(endpoint: Network) -> Result<Self> {
        let tx_config = Some(solana_client::rpc_config::RpcSendTransactionConfig {
            skip_preflight: true,
            ..Default::default()
        });

        Ok(Self {
            endpoint,
            kps: Keys::new(),
            tx_config,
        })
    }

    pub fn set_config(&mut self, tx_config: RpcSendTransactionConfig) {
        self.tx_config = Some(tx_config);
    }
}

#[derive(Debug, Default)]
pub struct Keys(HashMap<String, Keypair>);

impl Keys {
    pub fn new() -> Self {
        Self(HashMap::new())
    }
    pub fn insert(&mut self, k: &str, v: Keypair) {
        self.0.insert(k.into(), v);
    }
    pub fn get(&self, k: &str) -> Result<&Keypair> {
        self.0.get(k).ok_or(Error::msg("missing key: {k}"))
    }

    pub fn get_pubkey(&self, k: &str) -> Result<Pubkey> {
        Ok(self.get(k)?.pubkey())
    }

    pub fn inner(&self) -> &HashMap<String, Keypair> {
        &self.0
    }

    pub fn inner_mut(&mut self) -> &mut HashMap<String, Keypair> {
        &mut self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Network {
    Localnet,
    Devnet,
    Mainnet,
    Custom(String),
}

impl ToString for Network {
    fn to_string(&self) -> String {
        use Network::*;
        match self {
            Localnet => LOCALHOST_URL,
            Devnet => DEVNET_URL,
            Mainnet => MAINNET_URL,
            Custom(s) => s,
        }
        .into()
    }
}

pub const DEVNET_URL: &str = "https://api.devnet.solana.com/";
pub const LOCALHOST_URL: &str = "https://localhost:8899/";
pub const MAINNET_URL: &str = "https://api.mainnet-beta.solana.com";

impl std::str::FromStr for Network {
    type Err = Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let network = match s {
            _ if s.to_lowercase() == "devnet" => Network::Devnet,
            _ if s == DEVNET_URL => Network::Devnet,

            _ if s.to_lowercase() == "localhost" => Network::Localnet,
            _ if s == LOCALHOST_URL => Network::Localnet,

            _ if s.to_lowercase() == "mainnet" => Network::Mainnet,
            _ if s == MAINNET_URL => Network::Mainnet,

            _ => Network::Custom(s.into()),
        };

        Ok(network)
    }
}

#[async_trait]
impl SolanaRpcInterface for Client {
    async fn send_and_confirm_transaction(&self, transaction: &Transaction) -> Result<Signature> {
        let rpc = self.conn.clone();
        let transaction = transaction.clone();
        let commitment = self.conn.commitment();
        let tx_config = self.ctx.tx_config.unwrap_or(RpcSendTransactionConfig {
            preflight_commitment: Some(commitment.commitment),
            ..Default::default()
        });

        Ok(tokio::task::spawn_blocking(move || {
            rpc.send_and_confirm_transaction_with_spinner_and_config(
                &transaction,
                commitment,
                tx_config,
            )
        })
        .await??)
    }

    async fn get_account(&self, address: &Pubkey) -> Result<Account> {
        let rpc = self.conn.clone();
        let address = *address;

        let acc = tokio::task::spawn_blocking(move || rpc.get_account(&address)).await??;

        Ok(acc)
    }

    async fn get_multiple_accounts(&self, pubkeys: &[Pubkey]) -> Result<Vec<Option<Account>>> {
        let rpc = self.conn.clone();
        let pubkeys = pubkeys.to_vec();

        Ok(tokio::task::spawn_blocking(move || rpc.get_multiple_accounts(&pubkeys)).await??)
    }

    async fn get_program_accounts(
        &self,
        program_id: &Pubkey,
        size: Option<usize>,
    ) -> Result<Vec<(Pubkey, Account)>> {
        let rpc = self.conn.clone();
        let program_id = *program_id;
        let filters = size.map(|s| vec![RpcFilterType::DataSize(s as u64)]);

        Ok(tokio::task::spawn_blocking(move || {
            rpc.get_program_accounts_with_config(
                &program_id,
                RpcProgramAccountsConfig {
                    filters,
                    account_config: RpcAccountInfoConfig {
                        encoding: Some(UiAccountEncoding::Base64Zstd),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
        })
        .await??)
    }

    async fn get_latest_blockhash(&self) -> Result<Hash> {
        let rpc = self.conn.clone();
        let blockhash = tokio::task::spawn_blocking(move || rpc.get_latest_blockhash()).await??;

        Ok(blockhash)
    }

    async fn get_minimum_balance_for_rent_exemption(&self, length: usize) -> Result<u64> {
        let rpc = self.conn.clone();

        Ok(
            tokio::task::spawn_blocking(move || rpc.get_minimum_balance_for_rent_exemption(length))
                .await??,
        )
    }

    async fn send_transaction(&self, transaction: &Transaction) -> Result<Signature> {
        let rpc = self.conn.clone();
        let tx = transaction.clone();

        Ok(tokio::task::spawn_blocking(move || rpc.send_transaction(&tx)).await??)
    }

    async fn get_signature_statuses(
        &self,
        signatures: &[Signature],
    ) -> Result<Vec<Option<TransactionStatus>>> {
        let sigs = signatures.to_vec();
        let rpc = self.conn.clone();

        let sigs = tokio::task::spawn_blocking(move || rpc.get_signature_statuses(&sigs))
            .await??
            .value;
        Ok(sigs)
    }
    async fn get_clock(&self) -> Option<Clock> {
        let rpc = self.conn.clone();

        let slot = tokio::task::spawn_blocking(move || rpc.get_slot())
            .await
            .ok()?
            .ok()?;
        let rpc = self.conn.clone();
        let unix_timestamp = tokio::task::spawn_blocking(move || rpc.get_block_time(slot))
            .await
            .ok()?
            .ok()?;

        Some(Clock {
            slot,
            unix_timestamp,
            ..Default::default() // epoch probably doesn't matter?
        })
    }

    fn set_clock(&self, _new_clock: Clock) -> Result<()> {
        Err(Error::msg(
            "Live connection. Setting the clock is not permissible",
        ))
    }
}

#[async_trait]
impl TransactionContext for Client {
    async fn sign_send_instructions(
        &self,
        instructions: &[Instruction],
        add_signers: Option<&[Arc<dyn AsyncSigner>]>,
    ) -> Result<Signature> {
        let msg = Message::new_with_blockhash(
            instructions,
            Some(&self.payer()),
            &self.get_latest_blockhash().await?,
        );
        let mut signatures = Vec::<(Pubkey, Signature)>::new();
        for key in msg.signer_keys() {
            for kp in self.ctx.kps.inner().into_iter().map(|(_, v)| v) {
                let pk = kp.pubkey();
                if pk == *key {
                    signatures.push((pk, kp.sign_message(&msg.serialize())));
                }
            }
            if let Some(kps) = add_signers {
                for kp in kps {
                    let pk = kp.pubkey();
                    if pk == *key {
                        signatures.push((pk, kp.sign_message(&msg.serialize())))
                    }
                }
            }
        }
        let mut tx = Transaction::new_unsigned(msg);
        let positions = tx.get_signing_keypair_positions(
            &signatures.iter().map(|(k, _)| *k).collect::<Vec<Pubkey>>(),
        )?;
        if positions.iter().any(|p| p.is_none()) {
            return Err(Error::msg("missing signer"));
        }
        let positions = positions.iter().map(|p| p.unwrap()).collect::<Vec<usize>>();
        let signatures = signatures
            .into_iter()
            .map(|(_, s)| s)
            .collect::<Vec<Signature>>();
        for i in 0..positions.len() {
            tx.signatures[positions[i]] = signatures[i];
        }

        self.send_and_confirm_transaction(&tx).await
    }

    fn payer(&self) -> Pubkey {
        self.ctx.kps.get_pubkey("payer").unwrap()
    }
}
