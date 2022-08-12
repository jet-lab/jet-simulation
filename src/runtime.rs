use std::{collections::HashMap, sync::Arc, sync::Mutex};

use anyhow::bail;
use async_trait::async_trait;
use lazy_static::lazy_static;

use solana_bpf_loader_program::serialization::serialize_parameters;
use solana_program_runtime::{ic_logger_msg, invoke_context::InvokeContext, stable_log};
use solana_runtime::{
    accounts_index::ScanConfig,
    bank::{Bank, TransactionLogCollectorFilter},
};
use solana_sdk::{
    account::{Account, ReadableAccount},
    clock::Clock,
    entrypoint::deserialize,
    feature_set::FeatureSet,
    genesis_config::GenesisConfig,
    hash::Hash,
    instruction::InstructionError,
    pubkey::Pubkey,
    rent::Rent,
    signature::Signature,
    transaction::Transaction,
};
use solana_transaction_status::{TransactionConfirmationStatus, TransactionStatus};

use crate::solana_rpc_api::SolanaRpcClient;

pub type Entrypoint = fn(*mut u8) -> u64;

/// Utility for testing programs with the Solana runtime in-memory
pub struct TestRuntime {
    bank: Arc<Bank>,
}

lazy_static! {
    static ref GLOBAL_PROGRAM_MAP: GlobalProgramMap = GlobalProgramMap::new();
}

impl TestRuntime {
    pub fn new(
        native_programs: impl IntoIterator<Item = (Pubkey, Entrypoint)>,
        sbf_programs: impl IntoIterator<Item = (Pubkey, Vec<u8>)>,
    ) -> Self {
        let mut bank = Bank::new_no_wallclock_throttle_for_tests(&GenesisConfig::new(&[], &[]));
        let features = Arc::make_mut(&mut bank.feature_set);

        let programs = native_programs.into_iter().collect::<Vec<_>>();
        let program_ids = programs.iter().map(|(k, _)| *k).collect::<Vec<_>>();

        GLOBAL_PROGRAM_MAP.insert(features, HashMap::from_iter(programs.into_iter()));

        for program_id in program_ids {
            let ix_processor_name = format!("test-runtime:{program_id}");
            bank.add_builtin(&ix_processor_name, &program_id, global_instruction_handler);
        }

        // Add commonly-used SPL programs
        for (program_id, account) in
            solana_program_test::programs::spl_programs(&Rent::default()).iter()
        {
            bank.store_account(program_id, account);
        }

        bank.transaction_log_collector_config
            .write()
            .unwrap()
            .filter = TransactionLogCollectorFilter::All;

        Self {
            bank: Arc::new(bank),
        }
    }
}

/// Map of program handlers for each test context
struct GlobalProgramMap(Mutex<Vec<(Pubkey, HashMap<Pubkey, Entrypoint>)>>);

impl GlobalProgramMap {
    fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }

    fn insert(&self, feature: &mut FeatureSet, map: HashMap<Pubkey, Entrypoint>) {
        let mut bank_list = self.0.lock().unwrap();
        let id = Pubkey::new_unique();

        assert!(!bank_list.iter().any(|(k, _)| *k == id));
        bank_list.push((id, map));

        feature.activate(&id, 0);
    }

    fn get_programs(&self, feature: &FeatureSet) -> Option<HashMap<Pubkey, Entrypoint>> {
        let bank_list = self.0.lock().unwrap();

        for (bank_id, programs) in bank_list.iter() {
            if feature.is_active(&bank_id) {
                return Some(programs.clone());
            }
        }

        None
    }
}

fn global_instruction_handler(
    _: usize,
    context: &mut InvokeContext,
) -> Result<(), InstructionError> {
    let programs = GLOBAL_PROGRAM_MAP
        .get_programs(&context.feature_set)
        .unwrap();

    let tx_context = &context.transaction_context;
    let ix_context = tx_context.get_current_instruction_context()?;

    let (mut memory, _) = serialize_parameters(tx_context, ix_context, true)?;
    let program_id = ix_context.get_last_program_key(&tx_context)?;
    let program_entrypoint = programs
        .get(program_id)
        .ok_or(InstructionError::IncorrectProgramId)?;

    let logs = context.get_log_collector();

    stable_log::program_invoke(&logs, program_id, context.get_stack_height());
    let result = program_entrypoint(&mut memory.as_slice_mut()[0]);
    match result {
        0 => (),
        err => {
            let new_err = u64::from(err).into();

            stable_log::program_failure(&logs, program_id, &new_err);
            return Err(new_err);
        }
    };
    stable_log::program_success(&logs, &program_id);

    let (_, account_infos, _) = unsafe { deserialize(&mut memory.as_slice_mut()[0]) };
    let account_map = account_infos
        .into_iter()
        .map(|a| (a.key, a))
        .collect::<HashMap<_, _>>();

    // Commit changes
    for i in 0..ix_context.get_number_of_instruction_accounts() {
        let mut db_account = ix_context.try_borrow_instruction_account(tx_context, i)?;

        if !db_account.is_writable() {
            continue;
        }

        let account_info = match account_map.get(db_account.get_key()) {
            None => continue,
            Some(info) => info,
        };

        if !db_account
            .can_data_be_resized(account_info.data_len())
            .is_ok()
        {
            ic_logger_msg!(
                &logs,
                "Instruction attempting to resize account beyond limits"
            );
            return Err(InstructionError::InvalidRealloc);
        }

        if db_account.can_data_be_changed().is_ok() {
            ic_logger_msg!(&logs, "Instruction attempting to modify read-only data");
            return Err(InstructionError::ReadonlyDataModified);
        }

        db_account.set_lamports(account_info.lamports())?;
        db_account.set_data(&*account_info.try_borrow_data().unwrap())?;
    }

    Ok(())
}

#[async_trait]
impl SolanaRpcClient for TestRuntime {
    async fn get_account(&self, address: &Pubkey) -> anyhow::Result<Option<Account>> {
        Ok(self.bank.get_account(address).map(|a| a.into()))
    }

    async fn get_multiple_accounts(
        &self,
        addresses: &[Pubkey],
    ) -> anyhow::Result<Vec<Option<Account>>> {
        Ok(addresses
            .iter()
            .map(|addr| self.bank.get_account(addr).map(|a| a.into()))
            .collect())
    }

    async fn get_latest_blockhash(&self) -> anyhow::Result<Hash> {
        Ok(self.bank.last_blockhash())
    }

    async fn get_minimum_balance_for_rent_exemption(&self, length: usize) -> anyhow::Result<u64> {
        Ok(Rent::default().minimum_balance(length))
    }

    async fn send_transaction(
        &self,
        transaction: &Transaction,
    ) -> anyhow::Result<solana_sdk::signature::Signature> {
        let signature = transaction.signatures[0];

        match self.bank.process_transaction_with_logs(transaction) {
            Ok(()) => Ok(signature),
            Err(e) => {
                eprintln!("tx error {signature}: {e:?}");
                let log_collector = self.bank.transaction_log_collector.read().unwrap();
                let maybelog = log_collector.logs.last().unwrap();

                eprintln!("{:#?}", maybelog.log_messages);
                bail!("failed")
            }
        }
    }

    async fn get_signature_statuses(
        &self,
        signatures: &[Signature],
    ) -> anyhow::Result<Vec<Option<TransactionStatus>>> {
        Ok(signatures
            .iter()
            .map(|s| {
                self.bank
                    .get_signature_status(s)
                    .map(|status| TransactionStatus {
                        slot: self.bank.slot(),
                        err: status.as_ref().err().cloned(),
                        confirmations: Some(1),
                        confirmation_status: status
                            .as_ref()
                            .ok()
                            .map(|_| TransactionConfirmationStatus::Processed),
                        status,
                    })
            })
            .collect())
    }

    async fn get_program_accounts(
        &self,
        program_id: &Pubkey,
        size: Option<usize>,
    ) -> anyhow::Result<Vec<(Pubkey, Account)>> {
        Ok(self
            .bank
            .get_program_accounts(program_id, &ScanConfig::default())?
            .into_iter()
            .filter_map(|(address, account)| match (size, account.data().len()) {
                (Some(target), length) if target != length => None,
                _ => Some((address, account.into())),
            })
            .collect())
    }

    async fn airdrop(&self, account: &Pubkey, amount: u64) -> anyhow::Result<()> {
        self.bank.deposit(account, amount)?;
        Ok(())
    }

    async fn get_clock(&self) -> anyhow::Result<Clock> {
        let sysvar = self
            .bank
            .get_account(&solana_sdk::sysvar::clock::ID)
            .unwrap();
        Ok(bincode::deserialize(&sysvar.data())?)
    }

    async fn set_clock(&self, new_clock: Clock) -> anyhow::Result<()> {
        self.bank.set_sysvar_for_tests(&new_clock);
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use solana_sdk::{
        native_token::LAMPORTS_PER_SOL,
        signature::{Keypair, Signer},
        system_transaction,
    };

    use super::*;

    #[tokio::test]
    async fn can_simulate_simple_transfer() {
        let rt = TestRuntime::new([], []);
        let source_wallet = Keypair::new();
        let dest_wallet = Keypair::new();

        rt.airdrop(&source_wallet.pubkey(), 421 * LAMPORTS_PER_SOL)
            .await
            .unwrap();

        let recent_blockhash = rt.get_latest_blockhash().await.unwrap();
        let transfer_tx = system_transaction::transfer(
            &source_wallet,
            &dest_wallet.pubkey(),
            420 * LAMPORTS_PER_SOL,
            recent_blockhash,
        );

        rt.send_transaction(&transfer_tx).await.unwrap();

        let dest_balance = rt
            .get_account(&dest_wallet.pubkey())
            .await
            .unwrap()
            .unwrap()
            .lamports;

        assert_eq!(420 * LAMPORTS_PER_SOL, dest_balance);
    }
}
