use std::{sync::Arc, time::Instant};

use cadence_macros::{statsd_count, statsd_time};
use jsonrpsee::{
    core::{async_trait, RpcResult},
    proc_macros::rpc,
    types::ErrorObjectOwned,
};
use serde::{Deserialize, Serialize};
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{clock::UnixTimestamp, transaction::VersionedTransaction};
use solana_transaction_status::UiTransactionEncoding;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::{
    errors::{invalid_request, AtlasTxnSenderError},
    transaction_store::{TransactionData, TransactionStore},
    txn_sender::TxnSender,
    vendor::solana_rpc::decode_and_deserialize,
};

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all(serialize = "camelCase", deserialize = "camelCase"))]
pub struct TransactionResult {
    pub signature: Option<String>,
    pub status: TransactionStatus,
    pub error: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all(serialize = "camelCase", deserialize = "camelCase"))]
pub enum TransactionStatus {
    Success,
    Failed,
    Skipped,
}

// jsonrpsee does not make it easy to access http data,
// so creating this optional param to pass in metadata
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all(serialize = "camelCase", deserialize = "camelCase"))]
pub struct RequestMetadata {
    pub api_key: String,
}

#[rpc(server)]
pub trait AtlasTxnSender {
    #[method(name = "health")]
    async fn health(&self) -> String;
    #[method(name = "sendTransaction")]
    async fn send_transaction(
        &self,
        txn: String,
        params: RpcSendTransactionConfig,
        request_metadata: Option<RequestMetadata>,
    ) -> RpcResult<String>;
    #[method(name = "sendTransactionBundle")]
    async fn send_transaction_bundle(
        &self,
        txns: Vec<String>,
        params: RpcSendTransactionConfig,
        request_metadata: Option<RequestMetadata>,
    ) -> RpcResult<Vec<TransactionResult>>;
}

pub struct AtlasTxnSenderImpl {
    txn_sender: Arc<dyn TxnSender>,
    transaction_store: Arc<dyn TransactionStore>,
    max_txn_send_retries: usize,
}

impl AtlasTxnSenderImpl {
    pub fn new(
        txn_sender: Arc<dyn TxnSender>,
        transaction_store: Arc<dyn TransactionStore>,
        max_txn_send_retries: usize,
    ) -> Self {
        Self {
            txn_sender,
            max_txn_send_retries,
            transaction_store,
        }
    }

    async fn send_and_confirm_transaction(
        &self,
        wire_transaction: Vec<u8>,
        versioned_transaction: VersionedTransaction,
        params: RpcSendTransactionConfig,
        request_metadata: Option<RequestMetadata>,
        api_key: String,
        index_str: String
    ) -> Result<(String, UnixTimestamp), (String, String)> {
        let sent_at = Instant::now();
        statsd_count!("send_and_confirm_transaction", 1, "api_key" => &api_key, "index" => &index_str);

        let signature = versioned_transaction.signatures[0].to_string();
        if self.transaction_store.has_signature(&signature) {
            statsd_count!("duplicate_transaction", 1, "api_key" => &api_key, "index" => &index_str);
            return self.wait_for_confirmation(&signature).await;
        }
        let transaction = TransactionData {
            wire_transaction,
            versioned_transaction,
            sent_at,
            retry_count: 0,
            max_retries: std::cmp::min(
                self.max_txn_send_retries,
                params.max_retries.unwrap_or(self.max_txn_send_retries),
            ),
            request_metadata,
        };
        self.txn_sender.send_transaction(transaction);
        statsd_time!("send_transaction_time", sent_at.elapsed(), "api_key" => &api_key, "index" => &index_str);
        self.wait_for_confirmation(&signature).await
    }

    // leverage on the existing confirm_transaction method
    async fn wait_for_confirmation(&self, signature: &str) -> Result<(String, UnixTimestamp), (String, String)> {
        let signature_str = signature.to_string();
        let solana_rpc = self.txn_sender.get_solana_rpc();

        match solana_rpc.confirm_transaction(signature_str.clone()).await {
            Some(block_time) => Ok((signature_str, block_time)),
            None => {
                let err_msg = format!("Transaction {} could not be confirmed in time", signature);
                Err((signature_str, err_msg))
            }
        }
    }

    fn check_blockhash_validity(
        &self,
        transaction: &VersionedTransaction
    ) -> Result<bool, AtlasTxnSenderError> {
        let blockhash = transaction.message.recent_blockhash();
        let leader_tracker = self.txn_sender.get_leader_tracker();
        leader_tracker.is_blockhash_valid(*blockhash)
    }
}

#[async_trait]
impl AtlasTxnSenderServer for AtlasTxnSenderImpl {
    async fn health(&self) -> String {
        "ok".to_string()
    }
    async fn send_transaction(
        &self,
        txn: String,
        params: RpcSendTransactionConfig,
        request_metadata: Option<RequestMetadata>,
    ) -> RpcResult<String> {
        let sent_at = Instant::now();
        let api_key = request_metadata
            .clone()
            .map(|m| m.api_key)
            .unwrap_or("none".to_string());
        statsd_count!("send_transaction", 1, "api_key" => &api_key);
        validate_send_transaction_params(&params)?;
        let start = Instant::now();
        let encoding = params.encoding.unwrap_or(UiTransactionEncoding::Base58);
        let binary_encoding = encoding.into_binary_encoding().ok_or_else(|| {
            invalid_request(&format!(
                "unsupported encoding: {encoding}. Supported encodings: base58, base64"
            ))
        })?;
        let (wire_transaction, versioned_transaction) =
            match decode_and_deserialize::<VersionedTransaction>(txn, binary_encoding) {
                Ok((wire_transaction, versioned_transaction)) => {
                    (wire_transaction, versioned_transaction)
                }
                Err(e) => {
                    return Err(invalid_request(&e.to_string()));
                }
            };
        let signature = versioned_transaction.signatures[0].to_string();
        if self.transaction_store.has_signature(&signature) {
            statsd_count!("duplicate_transaction", 1, "api_key" => &api_key);
            return Ok(signature);
        }
        let transaction = TransactionData {
            wire_transaction,
            versioned_transaction,
            sent_at,
            retry_count: 0,
            max_retries: std::cmp::min(
                self.max_txn_send_retries,
                params.max_retries.unwrap_or(self.max_txn_send_retries),
            ),
            request_metadata,
        };
        self.txn_sender.send_transaction(transaction);
        statsd_time!(
            "send_transaction_time",
            start.elapsed(),
            "api_key" => &api_key
        );
        Ok(signature)
    }

    async fn send_transaction_bundle(
        &self,
        txns: Vec<String>,
        params: RpcSendTransactionConfig,
        request_metadata: Option<RequestMetadata>,
    ) -> RpcResult<Vec<TransactionResult>> {
        let start = Instant::now();
        let api_key = request_metadata
            .clone()
            .map(|m| m.api_key.clone())
            .unwrap_or_else(|| "none".to_string());
        statsd_count!("send_transaction_bundle", 1, "api_key" => &api_key);
        validate_send_transaction_params(&params)?;

        let mut txns_result: Vec<TransactionResult> = Vec::new();
        let encoding = params.encoding.unwrap_or(UiTransactionEncoding::Base58);
        let binary_encoding = encoding.into_binary_encoding().ok_or_else(|| {
            invalid_request(&format!(
                "unsupported encoding: {encoding}. Supported encodings: base58, base64"
            ))
        })?;

        for (index, txn) in txns.iter().enumerate() {
            if index > 0 && txns_result.last().map_or(
                false, |r| r.status == TransactionStatus::Failed || r.status == TransactionStatus::Skipped) {
                txns_result.push(TransactionResult {
                    signature: None,
                    status: TransactionStatus::Skipped,
                    error: Some("Transaction skipped due to earlier failure or skip".to_string()),
                });
                continue;
            }

            let index_str = index.to_string();
            let decode_result = decode_and_deserialize::<VersionedTransaction>(txn.clone(), binary_encoding);

            if decode_result.is_err() {
                statsd_count!("transaction_decoding_failed", 1, "api_key" => &api_key, "index" => &index_str);
                txns_result.push(TransactionResult {
                    signature: None,
                    status: TransactionStatus::Failed,
                    error: Some(format!("Transaction decoding failed: {}", decode_result.err().unwrap())),
                });
                continue;
            }

            let (wire_transaction, versioned_transaction) = decode_result?;
            // info!("{:?}", wire_transaction.clone());
            // info!("{:?}", versioned_transaction.clone());
            match self.check_blockhash_validity(&versioned_transaction) {
                Ok(false) => {
                    statsd_count!("blockhash_expired", 1, "api_key" => &api_key, "index" => &index_str);
                    txns_result.push(TransactionResult {
                        signature: None,
                        status: TransactionStatus::Skipped,
                        error: Some("Transaction bundle processing stopped due to expired blockhash".to_string()),
                    });
                    continue;
                },
                Err(err) => {
                    warn!("Failed to check blockhash validity: {}", err);
                },
                _ => {} // continue processing
            }

            match self.send_and_confirm_transaction(
                wire_transaction.clone(),
                versioned_transaction.clone(),
                params.clone(),
                request_metadata.clone(),
                api_key.clone(),
                index_str.clone()
            ).await {
                Ok((signature, _)) => {
                    txns_result.push(TransactionResult {
                        signature: Some(signature),
                        status: TransactionStatus::Success,
                        error: None,
                    });
                },
                Err((signature, error_msg)) => {
                    txns_result.push(TransactionResult {
                        signature: if signature.is_empty() { None } else { Some(signature) },
                        status: TransactionStatus::Failed,
                        error: Some(error_msg),
                    });
                    continue;
                }
            }
        }

        statsd_time!(
            "send_transaction_bundle_time",
            start.elapsed(),
            "api_key" => &api_key
        );

        Ok(txns_result)
    }
}

fn validate_send_transaction_params(
    params: &RpcSendTransactionConfig,
) -> Result<(), ErrorObjectOwned> {
    if !params.skip_preflight {
        return Err(invalid_request("running preflight check is not supported"));
    }
    Ok(())
}
