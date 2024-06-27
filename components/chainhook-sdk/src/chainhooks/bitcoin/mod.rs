use super::types::{ChainhookInstance, ExactMatchingRule, HookAction, MatchingRule};
use crate::utils::Context;

use bitcoincore_rpc_json::bitcoin::{address::Payload, Address};
use chainhook_types::{
    BitcoinBlockData, BitcoinChainEvent, BitcoinNetwork, BitcoinTransactionData, BlockIdentifier,
    StacksBaseChainOperation, TransactionIdentifier,
};
use schemars::JsonSchema;

use hiro_system_kit::slog;

use miniscript::bitcoin::secp256k1::Secp256k1;
use miniscript::Descriptor;

use reqwest::{Client, Method};
use serde::{de, Deserialize, Deserializer};
use serde_json::Value as JsonValue;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    str::FromStr,
};

use reqwest::RequestBuilder;

use hex::FromHex;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct BitcoinChainhookSpecification {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_block: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_block: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expire_after_occurrence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_proof: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_inputs: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_outputs: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_witness: Option<bool>,
    #[serde(rename = "if_this")]
    pub predicate: BitcoinPredicateType,
    #[serde(rename = "then_that")]
    pub action: HookAction,
}

/// Maps some [BitcoinChainhookSpecification] to a corresponding [BitcoinNetwork]. This allows maintaining one
/// serialized predicate file for a given predicate on each network.
///
/// ### Examples
/// Given some file `predicate.json`:
/// ```json
/// {
///   "uuid": "my-id",
///   "name": "My Predicate",
///   "chain": "bitcoin",
///   "version": 1,
///   "networks": {
///     "regtest": {
///       // ...
///     },
///     "testnet": {
///       // ...
///     },
///     "mainnet": {
///       // ...
///     }
///   }
/// }
/// ```
/// You can deserialize the file to this type and create a [BitcoinChainhookInstance] for the desired network:
/// ```
/// use chainhook_sdk::chainhooks::bitcoin::BitcoinChainhookSpecificationNetworkMap;
/// use chainhook_sdk::chainhooks::bitcoin::BitcoinChainhookInstance;
/// use chainhook_types::BitcoinNetwork;
///
/// fn get_predicate(network: &BitcoinNetwork) -> Result<BitcoinChainhookInstance, String> {
///     let json_predicate =
///         std::fs::read_to_string("./predicate.json").expect("Unable to read file");
///     let hook_map: BitcoinChainhookSpecificationNetworkMap =
///         serde_json::from_str(&json_predicate).expect("Unable to parse Chainhook map");
///     hook_map.into_specification_for_network(network)
/// }
///
/// ```
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct BitcoinChainhookSpecificationNetworkMap {
    pub uuid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_uuid: Option<String>,
    pub name: String,
    pub version: u32,
    pub networks: BTreeMap<BitcoinNetwork, BitcoinChainhookSpecification>,
}

impl BitcoinChainhookSpecificationNetworkMap {
    pub fn into_specification_for_network(
        mut self,
        network: &BitcoinNetwork,
    ) -> Result<BitcoinChainhookInstance, String> {
        let spec = self
            .networks
            .remove(network)
            .ok_or("Network unknown".to_string())?;
        Ok(BitcoinChainhookInstance {
            uuid: self.uuid,
            owner_uuid: self.owner_uuid,
            name: self.name,
            network: network.clone(),
            version: self.version,
            start_block: spec.start_block,
            end_block: spec.end_block,
            blocks: spec.blocks,
            expire_after_occurrence: spec.expire_after_occurrence,
            predicate: spec.predicate,
            action: spec.action,
            include_proof: spec.include_proof.unwrap_or(false),
            include_inputs: spec.include_inputs.unwrap_or(false),
            include_outputs: spec.include_outputs.unwrap_or(false),
            include_witness: spec.include_witness.unwrap_or(false),
            enabled: false,
            expired_at: None,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BitcoinChainhookInstance {
    pub uuid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_uuid: Option<String>,
    pub name: String,
    pub network: BitcoinNetwork,
    pub version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_block: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_block: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expire_after_occurrence: Option<u64>,
    pub predicate: BitcoinPredicateType,
    pub action: HookAction,
    pub include_proof: bool,
    pub include_inputs: bool,
    pub include_outputs: bool,
    pub include_witness: bool,
    pub enabled: bool,
    pub expired_at: Option<u64>,
}

impl BitcoinChainhookInstance {
    pub fn key(&self) -> String {
        ChainhookInstance::bitcoin_key(&self.uuid)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct BitcoinTransactionFilterPredicate {
    pub predicate: BitcoinPredicateType,
}

impl BitcoinTransactionFilterPredicate {
    pub fn new(predicate: BitcoinPredicateType) -> BitcoinTransactionFilterPredicate {
        BitcoinTransactionFilterPredicate { predicate }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "scope")]
pub enum BitcoinPredicateType {
    Block,
    Txid(ExactMatchingRule),
    Inputs(InputPredicate),
    Outputs(OutputPredicate),
    StacksProtocol(StacksOperations),
    OrdinalsProtocol(OrdinalOperations),
}

pub struct BitcoinTriggerChainhook<'a> {
    pub chainhook: &'a BitcoinChainhookInstance,
    pub apply: Vec<(Vec<&'a BitcoinTransactionData>, &'a BitcoinBlockData)>,
    pub rollback: Vec<(Vec<&'a BitcoinTransactionData>, &'a BitcoinBlockData)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BitcoinTransactionPayload {
    #[serde(flatten)]
    pub block: BitcoinBlockData,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BitcoinChainhookPayload {
    pub uuid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InputPredicate {
    Txid(TxinPredicate),
    WitnessScript(MatchingRule),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutputPredicate {
    OpReturn(MatchingRule),
    P2pkh(ExactMatchingRule),
    P2sh(ExactMatchingRule),
    P2wpkh(ExactMatchingRule),
    P2wsh(ExactMatchingRule),
    Descriptor(DescriptorMatchingRule),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "operation")]
pub enum StacksOperations {
    StackerRewarded,
    BlockCommitted,
    LeaderRegistered,
    StxTransferred,
    StxLocked,
}

#[derive(Clone, Debug, Serialize, Deserialize, Hash, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum OrdinalsMetaProtocol {
    All,
    #[serde(rename = "brc-20")]
    Brc20,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct InscriptionFeedData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta_protocols: Option<HashSet<OrdinalsMetaProtocol>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "operation")]
pub enum OrdinalOperations {
    InscriptionFeed(InscriptionFeedData),
}

pub fn get_stacks_canonical_magic_bytes(network: &BitcoinNetwork) -> [u8; 2] {
    match network {
        BitcoinNetwork::Mainnet => *b"X2",
        BitcoinNetwork::Testnet => *b"T2",
        BitcoinNetwork::Regtest => *b"id",
        BitcoinNetwork::Signet => unreachable!(),
    }
}

pub struct PoxConfig {
    pub genesis_block_height: u64,
    pub prepare_phase_len: u64,
    pub reward_phase_len: u64,
    pub rewarded_addresses_per_block: usize,
}

impl PoxConfig {
    pub fn get_pox_cycle_len(&self) -> u64 {
        self.prepare_phase_len + self.reward_phase_len
    }

    pub fn get_pox_cycle_id(&self, block_height: u64) -> u64 {
        (block_height.saturating_sub(self.genesis_block_height)) / self.get_pox_cycle_len()
    }

    pub fn get_pos_in_pox_cycle(&self, block_height: u64) -> u64 {
        (block_height.saturating_sub(self.genesis_block_height)) % self.get_pox_cycle_len()
    }

    pub fn get_burn_address(&self) -> &str {
        match self.genesis_block_height {
            666050 => "1111111111111111111114oLvT2",
            2000000 => "burn-address-regtest",
            _ => "burn-address",
        }
    }
}
const POX_CONFIG_MAINNET: PoxConfig = PoxConfig {
    genesis_block_height: 666050,
    prepare_phase_len: 100,
    reward_phase_len: 2100,
    rewarded_addresses_per_block: 2,
};

const POX_CONFIG_TESTNET: PoxConfig = PoxConfig {
    genesis_block_height: 2000000,
    prepare_phase_len: 50,
    reward_phase_len: 1050,
    rewarded_addresses_per_block: 2,
};

const POX_CONFIG_DEVNET: PoxConfig = PoxConfig {
    genesis_block_height: 100,
    prepare_phase_len: 4,
    reward_phase_len: 10,
    rewarded_addresses_per_block: 2,
};

pub fn get_canonical_pox_config(network: &BitcoinNetwork) -> PoxConfig {
    match network {
        BitcoinNetwork::Mainnet => POX_CONFIG_MAINNET,
        BitcoinNetwork::Testnet => POX_CONFIG_TESTNET,
        BitcoinNetwork::Regtest => POX_CONFIG_DEVNET,
        BitcoinNetwork::Signet => unreachable!(),
    }
}

#[derive(Debug, Clone, PartialEq)]
#[repr(u8)]
pub enum StacksOpcodes {
    BlockCommit = b'[',
    KeyRegister = b'^',
    StackStx = b'x',
    PreStx = b'p',
    TransferStx = b'$',
}

impl TryFrom<u8> for StacksOpcodes {
    type Error = ();

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            x if x == StacksOpcodes::BlockCommit as u8 => Ok(StacksOpcodes::BlockCommit),
            x if x == StacksOpcodes::KeyRegister as u8 => Ok(StacksOpcodes::KeyRegister),
            x if x == StacksOpcodes::StackStx as u8 => Ok(StacksOpcodes::StackStx),
            x if x == StacksOpcodes::PreStx as u8 => Ok(StacksOpcodes::PreStx),
            x if x == StacksOpcodes::TransferStx as u8 => Ok(StacksOpcodes::TransferStx),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct TxinPredicate {
    pub txid: String,
    pub vout: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct DescriptorMatchingRule {
    // expression defines the bitcoin descriptor.
    pub expression: String,
    #[serde(default, deserialize_with = "deserialize_descriptor_range")]
    pub range: Option<[u32; 2]>,
}

// deserialize_descriptor_range makes sure that the range value is valid.
fn deserialize_descriptor_range<'de, D>(deserializer: D) -> Result<Option<[u32; 2]>, D::Error>
where
    D: Deserializer<'de>,
{
    let range: [u32; 2] = Deserialize::deserialize(deserializer)?;
    if !(range[0] < range[1]) {
        Err(de::Error::custom(
            "First element of 'range' must be lower than the second element",
        ))
    } else {
        Ok(Some(range))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BitcoinChainhookOccurrencePayload {
    pub apply: Vec<BitcoinTransactionPayload>,
    pub rollback: Vec<BitcoinTransactionPayload>,
    pub chainhook: BitcoinChainhookPayload,
}

impl BitcoinChainhookOccurrencePayload {
    pub fn from_trigger<'a>(
        trigger: BitcoinTriggerChainhook<'a>,
    ) -> BitcoinChainhookOccurrencePayload {
        BitcoinChainhookOccurrencePayload {
            apply: trigger
                .apply
                .into_iter()
                .map(|(transactions, block)| {
                    let mut block = block.clone();
                    block.transactions = transactions
                        .into_iter()
                        .map(|t| t.clone())
                        .collect::<Vec<_>>();
                    BitcoinTransactionPayload { block }
                })
                .collect::<Vec<_>>(),
            rollback: trigger
                .rollback
                .into_iter()
                .map(|(transactions, block)| {
                    let mut block = block.clone();
                    block.transactions = transactions
                        .into_iter()
                        .map(|t| t.clone())
                        .collect::<Vec<_>>();
                    BitcoinTransactionPayload { block }
                })
                .collect::<Vec<_>>(),
            chainhook: BitcoinChainhookPayload {
                uuid: trigger.chainhook.uuid.clone(),
            },
        }
    }
}

pub enum BitcoinChainhookOccurrence {
    Http(RequestBuilder, BitcoinChainhookOccurrencePayload),
    File(String, Vec<u8>),
    Data(BitcoinChainhookOccurrencePayload),
}

pub fn evaluate_bitcoin_chainhooks_on_chain_event<'a>(
    chain_event: &'a BitcoinChainEvent,
    active_chainhooks: &Vec<&'a BitcoinChainhookInstance>,
    ctx: &Context,
) -> (
    Vec<BitcoinTriggerChainhook<'a>>,
    BTreeMap<&'a str, &'a BlockIdentifier>,
    BTreeMap<&'a str, &'a BlockIdentifier>,
) {
    let mut evaluated_predicates = BTreeMap::new();
    let mut triggered_predicates = vec![];
    let mut expired_predicates = BTreeMap::new();

    match chain_event {
        BitcoinChainEvent::ChainUpdatedWithBlocks(event) => {
            for chainhook in active_chainhooks.iter() {
                let mut apply = vec![];
                let rollback = vec![];
                let end_block = chainhook.end_block.unwrap_or(u64::MAX);

                for block in event.new_blocks.iter() {
                    evaluated_predicates.insert(chainhook.uuid.as_str(), &block.block_identifier);
                    if end_block >= block.block_identifier.index {
                        let mut hits = vec![];
                        for tx in block.transactions.iter() {
                            if chainhook.predicate.evaluate_transaction_predicate(&tx, ctx) {
                                hits.push(tx);
                            }
                        }
                        if hits.len() > 0 {
                            apply.push((hits, block));
                        }
                    } else {
                        expired_predicates.insert(chainhook.uuid.as_str(), &block.block_identifier);
                    }
                }

                if !apply.is_empty() {
                    triggered_predicates.push(BitcoinTriggerChainhook {
                        chainhook,
                        apply,
                        rollback,
                    })
                }
            }
        }
        BitcoinChainEvent::ChainUpdatedWithReorg(event) => {
            for chainhook in active_chainhooks.iter() {
                let mut apply = vec![];
                let mut rollback = vec![];
                let end_block = chainhook.end_block.unwrap_or(u64::MAX);

                for block in event.blocks_to_rollback.iter() {
                    if end_block >= block.block_identifier.index {
                        let mut hits = vec![];
                        for tx in block.transactions.iter() {
                            if chainhook.predicate.evaluate_transaction_predicate(&tx, ctx) {
                                hits.push(tx);
                            }
                        }
                        if hits.len() > 0 {
                            rollback.push((hits, block));
                        }
                    } else {
                        expired_predicates.insert(chainhook.uuid.as_str(), &block.block_identifier);
                    }
                }
                for block in event.blocks_to_apply.iter() {
                    evaluated_predicates.insert(chainhook.uuid.as_str(), &block.block_identifier);
                    if end_block >= block.block_identifier.index {
                        let mut hits = vec![];
                        for tx in block.transactions.iter() {
                            if chainhook.predicate.evaluate_transaction_predicate(&tx, ctx) {
                                hits.push(tx);
                            }
                        }
                        if hits.len() > 0 {
                            apply.push((hits, block));
                        }
                    } else {
                        expired_predicates.insert(chainhook.uuid.as_str(), &block.block_identifier);
                    }
                }
                if !apply.is_empty() || !rollback.is_empty() {
                    triggered_predicates.push(BitcoinTriggerChainhook {
                        chainhook,
                        apply,
                        rollback,
                    })
                }
            }
        }
    }
    (
        triggered_predicates,
        evaluated_predicates,
        expired_predicates,
    )
}

pub fn serialize_bitcoin_payload_to_json<'a>(
    trigger: &BitcoinTriggerChainhook<'a>,
    proofs: &HashMap<&'a TransactionIdentifier, String>,
) -> JsonValue {
    let predicate_spec = trigger.chainhook;
    json!({
        "apply": trigger.apply.iter().map(|(transactions, block)| {
            json!({
                "block_identifier": block.block_identifier,
                "parent_block_identifier": block.parent_block_identifier,
                "timestamp": block.timestamp,
                "transactions": serialize_bitcoin_transactions_to_json(&predicate_spec, &transactions, proofs),
                "metadata": block.metadata,
            })
        }).collect::<Vec<_>>(),
        "rollback": trigger.rollback.iter().map(|(transactions, block)| {
            json!({
                "block_identifier": block.block_identifier,
                "parent_block_identifier": block.parent_block_identifier,
                "timestamp": block.timestamp,
                "transactions": serialize_bitcoin_transactions_to_json(&predicate_spec, &transactions, proofs),
                "metadata": block.metadata,
            })
        }).collect::<Vec<_>>(),
        "chainhook": {
            "uuid": trigger.chainhook.uuid,
            "predicate": trigger.chainhook.predicate,
            "is_streaming_blocks": trigger.chainhook.enabled
        }
    })
}

pub fn serialize_bitcoin_transactions_to_json<'a>(
    predicate_spec: &BitcoinChainhookInstance,
    transactions: &Vec<&BitcoinTransactionData>,
    proofs: &HashMap<&'a TransactionIdentifier, String>,
) -> Vec<JsonValue> {
    transactions
        .into_iter()
        .map(|transaction| {
            let mut metadata = serde_json::Map::new();

            metadata.insert("fee".into(), json!(transaction.metadata.fee));
            metadata.insert("index".into(), json!(transaction.metadata.index));

            let inputs = if predicate_spec.include_inputs {
                transaction
                    .metadata
                    .inputs
                    .iter()
                    .map(|input| {
                        let witness = if predicate_spec.include_witness {
                            input.witness.clone()
                        } else {
                            vec![]
                        };
                        json!({
                            "previous_output": {
                                "txin": input.previous_output.txid.hash.to_string(),
                                "vout": input.previous_output.vout,
                                "value": input.previous_output.value,
                                "block_height": input.previous_output.block_height,
                            },
                            "script_sig": input.script_sig,
                            "sequence": input.sequence,
                            "witness": witness
                        })
                    })
                    .collect::<Vec<_>>()
            } else {
                vec![]
            };
            metadata.insert("inputs".into(), json!(inputs));

            let outputs = if predicate_spec.include_outputs {
                transaction.metadata.outputs.clone()
            } else {
                vec![]
            };
            metadata.insert("outputs".into(), json!(outputs));

            let stacks_ops = if transaction.metadata.stacks_operations.is_empty() {
                vec![]
            } else {
                transaction.metadata.stacks_operations.clone()
            };
            metadata.insert("stacks_operations".into(), json!(stacks_ops));

            let ordinals_ops = if transaction.metadata.ordinal_operations.is_empty() {
                vec![]
            } else {
                transaction.metadata.ordinal_operations.clone()
            };
            metadata.insert("ordinal_operations".into(), json!(ordinals_ops));

            if let Some(ref brc20) = transaction.metadata.brc20_operation {
                metadata.insert("brc20_operation".into(), json!(brc20));
            }

            metadata.insert(
                "proof".into(),
                json!(proofs.get(&transaction.transaction_identifier)),
            );
            json!({
                "transaction_identifier": transaction.transaction_identifier,
                "operations": transaction.operations,
                "metadata": metadata
            })
        })
        .collect::<Vec<_>>()
}

pub fn handle_bitcoin_hook_action<'a>(
    trigger: BitcoinTriggerChainhook<'a>,
    proofs: &HashMap<&'a TransactionIdentifier, String>,
) -> Result<BitcoinChainhookOccurrence, String> {
    match &trigger.chainhook.action {
        HookAction::HttpPost(http) => {
            let client = Client::builder()
                .build()
                .map_err(|e| format!("unable to build http client: {}", e.to_string()))?;
            let host = format!("{}", http.url);
            let method = Method::POST;
            let body = serde_json::to_vec(&serialize_bitcoin_payload_to_json(&trigger, proofs))
                .map_err(|e| format!("unable to serialize payload {}", e.to_string()))?;
            let request = client
                .request(method, &host)
                .header("Content-Type", "application/json")
                .header("Authorization", http.authorization_header.clone())
                .body(body);

            let data = BitcoinChainhookOccurrencePayload::from_trigger(trigger);
            Ok(BitcoinChainhookOccurrence::Http(request, data))
        }
        HookAction::FileAppend(disk) => {
            let bytes = serde_json::to_vec(&serialize_bitcoin_payload_to_json(&trigger, proofs))
                .map_err(|e| format!("unable to serialize payload {}", e.to_string()))?;
            Ok(BitcoinChainhookOccurrence::File(
                disk.path.to_string(),
                bytes,
            ))
        }
        HookAction::Noop => Ok(BitcoinChainhookOccurrence::Data(
            BitcoinChainhookOccurrencePayload::from_trigger(trigger),
        )),
    }
}

struct OpReturn(String);
impl OpReturn {
    fn from_string(hex: &String) -> Result<String, String> {
        // Remove the `0x` prefix if present so that we can call from_hex without errors.
        let hex = hex.strip_prefix("0x").unwrap_or(hex);

        // Parse the hex bytes.
        let bytes = Vec::<u8>::from_hex(hex).unwrap();
        match bytes.as_slice() {
            // An OpReturn is composed by:
            // - OP_RETURN 0x6a
            // - Data length <N> (ignored)
            // - The data
            [0x6a, _, rest @ ..] => Ok(hex::encode(rest)),
            _ => Err(String::from("not an OP_RETURN")),
        }
    }
}

impl BitcoinPredicateType {
    pub fn evaluate_transaction_predicate(
        &self,
        tx: &BitcoinTransactionData,
        ctx: &Context,
    ) -> bool {
        // TODO(lgalabru): follow-up on this implementation
        match &self {
            BitcoinPredicateType::Block => true,
            BitcoinPredicateType::Txid(ExactMatchingRule::Equals(txid)) => {
                tx.transaction_identifier.hash.eq(txid)
            }
            BitcoinPredicateType::Outputs(OutputPredicate::OpReturn(rule)) => {
                for output in tx.metadata.outputs.iter() {
                    // opret contains the op_return data section prefixed with `0x`.
                    let opret = match OpReturn::from_string(&output.script_pubkey) {
                        Ok(op) => op,
                        Err(_) => continue,
                    };

                    // encoded_pattern takes a predicate pattern and return its lowercase hex
                    // representation.
                    fn encoded_pattern(pattern: &str) -> String {
                        // If the pattern starts with 0x, return it in lowercase and without the 0x
                        // prefix.
                        if pattern.starts_with("0x") {
                            return pattern
                                .strip_prefix("0x")
                                .unwrap()
                                .to_lowercase()
                                .to_string();
                        }

                        // In this case it should be trated as ASCII so let's return its hex
                        // representation.
                        hex::encode(pattern)
                    }

                    match rule {
                        MatchingRule::StartsWith(pattern) => {
                            if opret.starts_with(&encoded_pattern(pattern)) {
                                return true;
                            }
                        }
                        MatchingRule::EndsWith(pattern) => {
                            if opret.ends_with(&encoded_pattern(pattern)) {
                                return true;
                            }
                        }
                        MatchingRule::Equals(pattern) => {
                            if opret.eq(&encoded_pattern(pattern)) {
                                return true;
                            }
                        }
                    }
                }
                false
            }
            BitcoinPredicateType::Outputs(OutputPredicate::P2pkh(ExactMatchingRule::Equals(
                encoded_address,
            )))
            | BitcoinPredicateType::Outputs(OutputPredicate::P2sh(ExactMatchingRule::Equals(
                encoded_address,
            ))) => {
                let address = match Address::from_str(encoded_address) {
                    Ok(address) => address.assume_checked(),
                    Err(_) => return false,
                };
                let address_bytes = hex::encode(address.script_pubkey().as_bytes());
                for output in tx.metadata.outputs.iter() {
                    if output.script_pubkey[2..] == address_bytes {
                        return true;
                    }
                }
                false
            }
            BitcoinPredicateType::Outputs(OutputPredicate::P2wpkh(ExactMatchingRule::Equals(
                encoded_address,
            )))
            | BitcoinPredicateType::Outputs(OutputPredicate::P2wsh(ExactMatchingRule::Equals(
                encoded_address,
            ))) => {
                let address = match Address::from_str(encoded_address) {
                    Ok(address) => {
                        let checked_address = address.assume_checked();
                        match checked_address.payload() {
                            Payload::WitnessProgram(_) => checked_address,
                            _ => return false,
                        }
                    }
                    Err(_) => return false,
                };
                let address_bytes = hex::encode(address.script_pubkey().as_bytes());
                for output in tx.metadata.outputs.iter() {
                    if output.script_pubkey[2..] == address_bytes {
                        return true;
                    }
                }
                false
            }
            BitcoinPredicateType::Outputs(OutputPredicate::Descriptor(
                DescriptorMatchingRule { expression, range },
            )) => {
                // To derive from descriptors, we need to provide a secp context.
                let (sig, ver) = (&Secp256k1::signing_only(), &Secp256k1::verification_only());
                let (desc, _) = Descriptor::parse_descriptor(&sig, expression).unwrap();

                // If the descriptor is derivable (`has_wildcard()`), we rely on the `range` field
                // defined by the predicate OR fallback to a default range of [0,5] when not set.
                // When the descriptor is not derivable we force to create a unique iteration by
                // ranging over [0,1].
                let range = if desc.has_wildcard() {
                    range.unwrap_or([0, 5])
                } else {
                    [0, 1]
                };

                // Derive the addresses and try to match them against the outputs.
                for i in range[0]..range[1] {
                    let derived = desc.derived_descriptor(&ver, i).unwrap();

                    // Extract and encode the derived pubkey.
                    let script_pubkey = hex::encode(derived.script_pubkey().as_bytes());

                    // Match that script against the tx outputs.
                    for (index, output) in tx.metadata.outputs.iter().enumerate() {
                        if output.script_pubkey[2..] == script_pubkey {
                            ctx.try_log(|logger| {
                                slog::debug!(
                                    logger,
                                    "Descriptor: Matched pubkey {:?} on tx {:?} output {}",
                                    script_pubkey,
                                    tx.transaction_identifier.get_hash_bytes_str(),
                                    index,
                                )
                            });

                            return true;
                        }
                    }
                }

                false
            }
            BitcoinPredicateType::Inputs(InputPredicate::Txid(predicate)) => {
                // TODO(lgalabru): add support for transaction chainhing, if enabled
                for input in tx.metadata.inputs.iter() {
                    if input.previous_output.txid.hash.eq(&predicate.txid)
                        && input.previous_output.vout.eq(&predicate.vout)
                    {
                        return true;
                    }
                }
                false
            }
            BitcoinPredicateType::Inputs(InputPredicate::WitnessScript(_)) => {
                // TODO(lgalabru)
                unimplemented!()
            }
            BitcoinPredicateType::StacksProtocol(StacksOperations::StackerRewarded) => {
                for op in tx.metadata.stacks_operations.iter() {
                    if let StacksBaseChainOperation::BlockCommitted(_) = op {
                        return true;
                    }
                }
                false
            }
            BitcoinPredicateType::StacksProtocol(StacksOperations::BlockCommitted) => {
                for op in tx.metadata.stacks_operations.iter() {
                    if let StacksBaseChainOperation::BlockCommitted(_) = op {
                        return true;
                    }
                }
                false
            }
            BitcoinPredicateType::StacksProtocol(StacksOperations::LeaderRegistered) => {
                for op in tx.metadata.stacks_operations.iter() {
                    if let StacksBaseChainOperation::LeaderRegistered(_) = op {
                        return true;
                    }
                }
                false
            }
            BitcoinPredicateType::StacksProtocol(StacksOperations::StxTransferred) => {
                for op in tx.metadata.stacks_operations.iter() {
                    if let StacksBaseChainOperation::StxTransferred(_) = op {
                        return true;
                    }
                }
                false
            }
            BitcoinPredicateType::StacksProtocol(StacksOperations::StxLocked) => {
                for op in tx.metadata.stacks_operations.iter() {
                    if let StacksBaseChainOperation::StxLocked(_) = op {
                        return true;
                    }
                }
                false
            }
            BitcoinPredicateType::OrdinalsProtocol(OrdinalOperations::InscriptionFeed(
                feed_data,
            )) => match &feed_data.meta_protocols {
                Some(meta_protocols) => {
                    for meta_protocol in meta_protocols.iter() {
                        match meta_protocol {
                            OrdinalsMetaProtocol::All => {
                                return !tx.metadata.ordinal_operations.is_empty()
                            }
                            OrdinalsMetaProtocol::Brc20 => {
                                return !tx.metadata.brc20_operation.is_none()
                            }
                        }
                    }
                    false
                }
                None => !tx.metadata.ordinal_operations.is_empty(),
            },
        }
    }
}

#[cfg(test)]
pub mod tests;
