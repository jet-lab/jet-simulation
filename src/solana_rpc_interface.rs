// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Copyright (C) 2022 JET PROTOCOL HOLDINGS, LLC.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use anyhow::Result;
use async_trait::async_trait;
use solana_sdk::{
    account::Account, clock::Clock, hash::Hash, pubkey::Pubkey, signature::Signature,
    transaction::Transaction,
};
use solana_transaction_status::TransactionStatus;

/// Represents some client interface to the Solana network.
#[async_trait]
pub trait SolanaRpcInterface: Send + Sync {
    async fn get_account(&self, address: &Pubkey) -> Result<Account>;
    async fn get_multiple_accounts(&self, pubkeys: &[Pubkey]) -> Result<Vec<Option<Account>>>;
    async fn get_latest_blockhash(&self) -> Result<Hash>;
    async fn get_minimum_balance_for_rent_exemption(&self, length: usize) -> Result<u64>;
    async fn send_transaction(&self, transaction: &Transaction) -> Result<Signature>;
    async fn get_signature_statuses(
        &self,
        signatures: &[Signature],
    ) -> Result<Vec<Option<TransactionStatus>>>;

    async fn get_program_accounts(
        &self,
        program_id: &Pubkey,
        size: Option<usize>,
    ) -> Result<Vec<(Pubkey, Account)>>;

    async fn send_and_confirm_transaction(&self, transaction: &Transaction) -> Result<Signature> {
        let signature = self.send_transaction(transaction).await?;
        let _ = self.confirm_transactions(&[signature]).await?;

        Ok(signature)
    }

    async fn confirm_transactions(&self, signatures: &[Signature]) -> Result<Vec<bool>> {
        for _ in 0..7 {
            let statuses = self.get_signature_statuses(signatures).await?;

            if !statuses.iter().all(|s| s.is_some()) {
                return Ok(statuses
                    .into_iter()
                    .map(|s| s.unwrap().err.is_none())
                    .collect());
            }
        }

        anyhow::bail!("failed to confirm signatures: {:?}", signatures);
    }
    async fn get_clock(&self) -> Option<Clock>;
    fn set_clock(&self, new_clock: Clock) -> Result<()>;
}
