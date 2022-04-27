// Copyright (c) 2021, Facebook, Inc. and its affiliates
// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use super::{base_types::*, batch::*, committee::Committee, error::*, event::Event};
use crate::committee::EpochId;
use crate::crypto::{
    sha3_hash, AuthoritySignInfo, AuthoritySignInfoTrait, AuthoritySignature, BcsSignable,
    EmptySignInfo, Signable, Signature, VerificationObligation,
};
use crate::gas::GasCostSummary;
use crate::json_schema;
use crate::object::{Object, ObjectFormatOptions, Owner, OBJECT_START_VERSION};
use crate::readable_serde::encoding::Base64;
use crate::readable_serde::Readable;
use base64ct::Encoding;
use itertools::Either;
use move_binary_format::access::ModuleAccess;
use move_binary_format::CompiledModule;
use move_core_types::{
    account_address::AccountAddress, identifier::Identifier, language_storage::TypeTag,
    value::MoveStructLayout,
};
use name_variant::NamedVariant;
use once_cell::sync::OnceCell;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_name::{DeserializeNameAdapter, SerializeNameAdapter};
use serde_with::serde_as;
use serde_with::Bytes;
use std::fmt::Write;
use std::fmt::{Display, Formatter};
use std::{
    collections::{BTreeSet, HashSet},
    hash::{Hash, Hasher},
};
#[cfg(test)]
#[path = "unit_tests/messages_tests.rs"]
mod messages_tests;

#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize, JsonSchema)]
pub enum CallArg {
    // contains no structs or objects
    Pure(Vec<u8>),
    // TODO support more than one object (object vector of some sort)
    // A Move object, either immutable, or owned mutable.
    ImmOrOwnedObject(ObjectRef),
    // A Move object that's shared and mutable.
    SharedObject(ObjectID),
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Transfer {
    pub recipient: SuiAddress,
    pub object_ref: ObjectRef,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MoveCall {
    // Although `package` represents a read-only Move package,
    // we still want to use a reference instead of just object ID.
    // This allows a client to be able to validate the package object
    // used in an order (through the object digest) without having to
    // re-execute the order on a quorum of authorities.
    pub package: ObjectRef,
    #[schemars(with = "json_schema::Identifier")]
    pub module: Identifier,
    #[schemars(with = "json_schema::Identifier")]
    pub function: Identifier,
    #[schemars(with = "Vec<json_schema::TypeTag>")]
    pub type_arguments: Vec<TypeTag>,
    pub arguments: Vec<CallArg>,
}

#[serde_as]
#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MoveModulePublish {
    #[schemars(with = "Vec<String>")]
    #[serde_as(as = "Vec<Readable<Base64, Bytes>>")]
    pub modules: Vec<Vec<u8>>,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize, JsonSchema)]
pub enum SingleTransactionKind {
    /// Initiate an object transfer between addresses
    Transfer(Transfer),
    /// Publish a new Move module
    Publish(MoveModulePublish),
    /// Call a function in a published Move module
    Call(MoveCall),
    // .. more transaction types go here
}

impl SingleTransactionKind {
    pub fn contains_shared_object(&self) -> bool {
        self.shared_input_objects().next().is_some()
    }

    pub fn shared_input_objects(&self) -> impl Iterator<Item = &ObjectID> {
        match &self {
            Self::Call(MoveCall { arguments, .. }) => {
                Either::Left(arguments.iter().filter_map(|arg| match arg {
                    CallArg::Pure(_) | CallArg::ImmOrOwnedObject(_) => None,
                    CallArg::SharedObject(id) => Some(id),
                }))
            }
            _ => Either::Right(std::iter::empty()),
        }
    }

    /// Return the metadata of each of the input objects for the transaction.
    /// For a Move object, we attach the object reference;
    /// for a Move package, we provide the object id only since they never change on chain.
    /// TODO: use an iterator over references here instead of a Vec to avoid allocations.
    pub fn input_objects(&self) -> SuiResult<Vec<InputObjectKind>> {
        let input_objects = match &self {
            Self::Transfer(Transfer { object_ref, .. }) => {
                vec![InputObjectKind::ImmOrOwnedMoveObject(*object_ref)]
            }
            Self::Call(MoveCall {
                arguments, package, ..
            }) => arguments
                .iter()
                .filter_map(|arg| match arg {
                    CallArg::Pure(_) => None,
                    CallArg::ImmOrOwnedObject(object_ref) => {
                        Some(InputObjectKind::ImmOrOwnedMoveObject(*object_ref))
                    }
                    CallArg::SharedObject(id) => Some(InputObjectKind::SharedMoveObject(*id)),
                })
                .chain([InputObjectKind::MovePackage(package.0)])
                .collect(),
            Self::Publish(MoveModulePublish { modules }) => {
                // For module publishing, all the dependent packages are implicit input objects
                // because they must all be on-chain in order for the package to publish.
                // All authorities must have the same view of those dependencies in order
                // to achieve consistent publish results.
                let compiled_modules = modules
                    .iter()
                    .filter_map(|bytes| match CompiledModule::deserialize(bytes) {
                        Ok(m) => Some(m),
                        // We will ignore this error here and simply let latter execution
                        // to discover this error again and fail the transaction.
                        // It's preferable to let transaction fail and charge gas when
                        // malformed package is provided.
                        Err(_) => None,
                    })
                    .collect::<Vec<_>>();
                Transaction::input_objects_in_compiled_modules(&compiled_modules)
            }
        };
        // Ensure that there are no duplicate inputs. This cannot be removed because:
        // In [`AuthorityState::check_locks`], we check that there are no duplicate mutable
        // input objects, which would have made this check here unnecessary. However we
        // do plan to allow shared objects show up more than once in multiple single
        // transactions down the line. Once we have that, we need check here to make sure
        // the same shared object doesn't show up more than once in the same single
        // transaction.
        let mut used = HashSet::new();
        if !input_objects.iter().all(|o| used.insert(o.object_id())) {
            return Err(SuiError::DuplicateObjectRefInput);
        }
        Ok(input_objects)
    }
}

impl Display for SingleTransactionKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut writer = String::new();
        match &self {
            Self::Transfer(t) => {
                writeln!(writer, "Transaction Kind : Transfer")?;
                writeln!(writer, "Recipient : {}", t.recipient)?;
                let (object_id, seq, digest) = t.object_ref;
                writeln!(writer, "Object ID : {}", &object_id)?;
                writeln!(writer, "Sequence Number : {:?}", seq)?;
                writeln!(writer, "Object Digest : {}", encode_bytes_hex(&digest.0))?;
            }
            Self::Publish(_p) => {
                writeln!(writer, "Transaction Kind : Publish")?;
            }
            Self::Call(c) => {
                writeln!(writer, "Transaction Kind : Call")?;
                writeln!(writer, "Package ID : {}", c.package.0.to_hex_literal())?;
                writeln!(writer, "Module : {}", c.module)?;
                writeln!(writer, "Function : {}", c.function)?;
                writeln!(writer, "Arguments : {:?}", c.arguments)?;
                writeln!(writer, "Type Arguments : {:?}", c.type_arguments)?;
            }
        }
        write!(f, "{}", writer)
    }
}

// TODO: Make SingleTransactionKind a Box
#[allow(clippy::large_enum_variant)]
#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize, NamedVariant, JsonSchema)]
pub enum TransactionKind {
    /// A single transaction.
    Single(SingleTransactionKind),
    /// A batch of single transactions.
    Batch(Vec<SingleTransactionKind>),
    // .. more transaction types go here
}

impl Display for TransactionKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut writer = String::new();
        match &self {
            Self::Single(s) => {
                writeln!(writer, "{}", s)?;
            }
            Self::Batch(b) => {
                writeln!(writer, "Transaction Kind : Batch")?;
                writeln!(writer, "List of transactions in the batch:")?;
                for kind in b {
                    writeln!(writer, "{}", kind)?;
                }
            }
        }
        write!(f, "{}", writer)
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TransactionData {
    pub kind: TransactionKind,
    sender: SuiAddress,
    gas_payment: ObjectRef,
    pub gas_budget: u64,
}

impl TransactionData
where
    Self: BcsSignable,
{
    pub fn new(
        kind: TransactionKind,
        sender: SuiAddress,
        gas_payment: ObjectRef,
        gas_budget: u64,
    ) -> Self {
        TransactionData {
            kind,
            sender,
            gas_payment,
            gas_budget,
        }
    }

    pub fn new_move_call(
        sender: SuiAddress,
        package: ObjectRef,
        module: Identifier,
        function: Identifier,
        type_arguments: Vec<TypeTag>,
        gas_payment: ObjectRef,
        arguments: Vec<CallArg>,
        gas_budget: u64,
    ) -> Self {
        let kind = TransactionKind::Single(SingleTransactionKind::Call(MoveCall {
            package,
            module,
            function,
            type_arguments,
            arguments,
        }));
        Self::new(kind, sender, gas_payment, gas_budget)
    }

    pub fn new_transfer(
        recipient: SuiAddress,
        object_ref: ObjectRef,
        sender: SuiAddress,
        gas_payment: ObjectRef,
        gas_budget: u64,
    ) -> Self {
        let kind = TransactionKind::Single(SingleTransactionKind::Transfer(Transfer {
            recipient,
            object_ref,
        }));
        Self::new(kind, sender, gas_payment, gas_budget)
    }

    pub fn new_module(
        sender: SuiAddress,
        gas_payment: ObjectRef,
        modules: Vec<Vec<u8>>,
        gas_budget: u64,
    ) -> Self {
        let kind = TransactionKind::Single(SingleTransactionKind::Publish(MoveModulePublish {
            modules,
        }));
        Self::new(kind, sender, gas_payment, gas_budget)
    }

    /// Returns the transaction kind as a &str (variant name, no fields)
    pub fn kind_as_str(&self) -> &'static str {
        self.kind.variant_name()
    }

    pub fn gas(&self) -> ObjectRef {
        self.gas_payment
    }

    pub fn signer(&self) -> SuiAddress {
        self.sender
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut writer = Vec::new();
        self.write(&mut writer);
        writer
    }

    pub fn to_base64(&self) -> String {
        base64ct::Base64::encode_string(&self.to_bytes())
    }
}

/// A transaction signed by a client, optionally signed by an authority (depending on `S`).
/// `S` indicates the authority signing state. It can be either empty or signed.
/// We make the authority signature templated so that `TransactionEnvelope<S>` can be used
/// universally in the transactions storage in `SuiDataStore`, shared by both authorities
/// and non-authorities: authorities store signed transactions, while non-authorities
/// store unsigned transactions.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(remote = "TransactionEnvelope")]
pub struct TransactionEnvelope<S> {
    // Deserialization sets this to "false"
    #[serde(skip)]
    pub is_checked: bool,

    pub data: TransactionData,
    /// tx_signature is signed by the transaction sender, applied on `data`.
    pub tx_signature: Signature,
    /// auth_signature, if available, is signed by an authority, applied on `data`.
    pub auth_signature: S,
    // Note: If any new field is added here, make sure the Hash and PartialEq
    // implementation are adjusted to include that new field (unless the new field
    // does not participate in the hash and comparison).
}

impl<S: AuthoritySignInfoTrait> TransactionEnvelope<S> {
    pub fn check_signature(&self) -> Result<(), SuiError> {
        // We use this flag to see if someone has checked this before
        // and therefore we can skip the check. Note that the flag has
        // to be set to true manually, and is not set by calling this
        // "check" function.
        if self.is_checked {
            return Ok(());
        }

        let mut obligation = VerificationObligation::default();
        self.add_to_verification_obligation(&mut obligation)?;
        obligation.verify_all().map(|_| ())
    }

    pub fn add_to_verification_obligation(
        &self,
        obligation: &mut VerificationObligation,
    ) -> SuiResult<()> {
        let (message, signature, public_key) = self
            .tx_signature
            .get_verification_inputs(&self.data, self.data.sender)?;
        let idx = obligation.messages.len();
        obligation.messages.push(message);
        let key = obligation.lookup_public_key(&public_key)?;
        obligation.public_keys.push(key);
        obligation.signatures.push(signature);
        obligation.message_index.push(idx);
        Ok(())
    }

    pub fn sender_address(&self) -> SuiAddress {
        self.data.sender
    }

    pub fn gas_payment_object_ref(&self) -> &ObjectRef {
        &self.data.gas_payment
    }

    pub fn contains_shared_object(&self) -> bool {
        self.shared_input_objects().next().is_some()
    }

    pub fn shared_input_objects(&self) -> impl Iterator<Item = &ObjectID> {
        match &self.data.kind {
            TransactionKind::Single(s) => Either::Left(s.shared_input_objects()),
            TransactionKind::Batch(b) => {
                Either::Right(b.iter().flat_map(|kind| kind.shared_input_objects()))
            }
        }
    }

    pub fn input_objects(&self) -> SuiResult<Vec<InputObjectKind>> {
        let mut inputs = match &self.data.kind {
            TransactionKind::Single(s) => s.input_objects()?,
            TransactionKind::Batch(b) => {
                let mut result = vec![];
                for kind in b {
                    fp_ensure!(
                        !matches!(kind, &SingleTransactionKind::Publish(..)),
                        SuiError::InvalidBatchTransaction {
                            error: "Publish transaction is not allowed in Batch Transaction"
                                .to_owned(),
                        }
                    );
                    let sub = kind.input_objects()?;
                    result.extend(sub);
                }
                result
            }
        };
        inputs.push(InputObjectKind::ImmOrOwnedMoveObject(
            *self.gas_payment_object_ref(),
        ));
        Ok(inputs)
    }

    pub fn single_transactions(&self) -> impl Iterator<Item = &SingleTransactionKind> {
        match &self.data.kind {
            TransactionKind::Single(s) => Either::Left(std::iter::once(s)),
            TransactionKind::Batch(b) => Either::Right(b.iter()),
        }
    }

    pub fn into_single_transactions(self) -> impl Iterator<Item = SingleTransactionKind> {
        match self.data.kind {
            TransactionKind::Single(s) => Either::Left(std::iter::once(s)),
            TransactionKind::Batch(b) => Either::Right(b.into_iter()),
        }
    }

    // Derive a cryptographic hash of the transaction.
    pub fn digest(&self) -> TransactionDigest {
        TransactionDigest::new(sha3_hash(&self.data))
    }

    pub fn input_objects_in_compiled_modules(
        compiled_modules: &[CompiledModule],
    ) -> Vec<InputObjectKind> {
        let mut dependent_packages = BTreeSet::new();
        for module in compiled_modules.iter() {
            for handle in module.module_handles.iter() {
                let address = ObjectID::from(*module.address_identifier_at(handle.address));
                if address != ObjectID::ZERO {
                    dependent_packages.insert(address);
                }
            }
        }

        // We don't care about the digest of the dependent packages.
        // They are all read-only on-chain and their digest never changes.
        dependent_packages
            .into_iter()
            .map(InputObjectKind::MovePackage)
            .collect::<Vec<_>>()
    }
}

// In combination with #[serde(remote = "TransactionEnvelope")].
// Generic types instantiated multiple times in the same tracing session requires a work around.
// https://novifinancial.github.io/serde-reflection/serde_reflection/index.html#features-and-limitations
impl<'de, T> Deserialize<'de> for TransactionEnvelope<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        TransactionEnvelope::deserialize(DeserializeNameAdapter::new(
            deserializer,
            // TODO: This generates a very long name that includes the namespace and modules.
            // Ideally we just want TransactionEnvelope<T> with T substituted as the name.
            // https://github.com/MystenLabs/sui/issues/1119
            std::any::type_name::<TransactionEnvelope<T>>(),
        ))
    }
}

impl<T> Serialize for TransactionEnvelope<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        TransactionEnvelope::serialize(
            self,
            SerializeNameAdapter::new(serializer, std::any::type_name::<TransactionEnvelope<T>>()),
        )
    }
}

// TODO: this should maybe be called ClientSignedTransaction + SignedTransaction -> AuthoritySignedTransaction
/// A transaction that is signed by a sender but not yet by an authority.
pub type Transaction = TransactionEnvelope<EmptySignInfo>;

impl Transaction {
    #[cfg(test)]
    pub fn from_data(data: TransactionData, signer: &dyn signature::Signer<Signature>) -> Self {
        let signature = Signature::new(&data, signer);
        Self::new(data, signature)
    }

    pub fn new(data: TransactionData, signature: Signature) -> Self {
        Self {
            is_checked: false,
            data,
            tx_signature: signature,
            auth_signature: EmptySignInfo {},
        }
    }
}

impl Hash for Transaction {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.data.hash(state);
    }
}

impl PartialEq for Transaction {
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data
    }
}
impl Eq for Transaction {}

/// A transaction that is signed by a sender and also by an authority.
pub type SignedTransaction = TransactionEnvelope<AuthoritySignInfo>;

impl SignedTransaction {
    /// Use signing key to create a signed object.
    pub fn new(
        epoch: EpochId,
        transaction: Transaction,
        authority: AuthorityName,
        secret: &dyn signature::Signer<AuthoritySignature>,
    ) -> Self {
        let signature = AuthoritySignature::new(&transaction.data, secret);
        Self {
            is_checked: transaction.is_checked,
            data: transaction.data,
            tx_signature: transaction.tx_signature,
            auth_signature: AuthoritySignInfo {
                epoch,
                authority,
                signature,
            },
        }
    }

    /// Verify the signature and return the non-zero voting right of the authority.
    pub fn check(&self, committee: &Committee) -> Result<usize, SuiError> {
        self.check_signature()?;
        let weight = committee.weight(&self.auth_signature.authority);
        fp_ensure!(weight > 0, SuiError::UnknownSigner);
        self.auth_signature
            .signature
            .check(&self.data, self.auth_signature.authority)?;
        Ok(weight)
    }

    // Turn a SignedTransaction into a Transaction. This is needed when we are
    // forming a CertifiedTransaction, where each transaction's authority signature
    // is taking out to form an aggregated signature.
    pub fn to_transaction(self) -> Transaction {
        Transaction::new(self.data, self.tx_signature)
    }
}

impl Hash for SignedTransaction {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.data.hash(state);
        self.auth_signature.authority.hash(state);
    }
}

impl PartialEq for SignedTransaction {
    fn eq(&self, other: &Self) -> bool {
        // We do not compare the signatures, because there can be multiple
        // valid signatures for the same data and signer.
        self.data == other.data && self.auth_signature.authority == other.auth_signature.authority
    }
}

/// An transaction signed by a quorum of authorities
///
/// Note: the signature set of this data structure is not necessarily unique in the system,
/// i.e. there can be several valid certificates per transaction.
///
/// As a consequence, we check this struct does not implement Hash or Eq, see the note below.
///
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CertifiedTransaction {
    // This is a cache of an otherwise expensive to compute value.
    // DO NOT serialize or deserialize from the network or disk.
    #[serde(skip)]
    transaction_digest: OnceCell<TransactionDigest>,
    // Deserialization sets this to "false"
    #[serde(skip)]
    pub is_checked: bool,

    pub epoch: EpochId,
    pub transaction: Transaction,
    pub signatures: Vec<(AuthorityName, AuthoritySignature)>,
}

// Note: if you meet an error due to this line it may be because you need an Eq implementation for `CertifiedTransaction`,
// or one of the structs that include it, i.e. `ConfirmationTransaction`, `TransactionInfoResponse` or `ObjectInfoResponse`.
//
// Please note that any such implementation must be agnostic to the exact set of signatures in the certificate, as
// clients are allowed to equivocate on the exact nature of valid certificates they send to the system. This assertion
// is a simple tool to make sure certificates are accounted for correctly - should you remove it, you're on your own to
// maintain the invariant that valid certificates with distinct signatures are equivalent, but yet-unchecked
// certificates that differ on signers aren't.
//
// see also https://github.com/MystenLabs/sui/issues/266
//
static_assertions::assert_not_impl_any!(CertifiedTransaction: Hash, Eq, PartialEq);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfirmationTransaction {
    pub certificate: CertifiedTransaction,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize)]
pub struct AccountInfoRequest {
    pub account: SuiAddress,
}

/// An information Request for batches, and their associated transactions
///
/// This reads historic data and sends the batch and transactions in the
/// database starting at the batch that includes `start`,
/// and then listens to new transactions until a batch equal or
/// is over the batch end marker.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize)]
pub struct BatchInfoRequest {
    pub start: TxSequenceNumber,
    pub end: TxSequenceNumber,
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct BatchInfoResponseItem(pub UpdateItem);

impl From<SuiAddress> for AccountInfoRequest {
    fn from(account: SuiAddress) -> Self {
        AccountInfoRequest { account }
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize)]
pub enum ObjectInfoRequestKind {
    /// Request the latest object state, if a format option is provided,
    /// return the layout of the object in the given format.
    LatestObjectInfo(Option<ObjectFormatOptions>),
    /// Request the object state at a specific version
    PastObjectInfo(SequenceNumber),
}

/// A request for information about an object and optionally its
/// parent certificate at a specific version.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize)]
pub struct ObjectInfoRequest {
    /// The id of the object to retrieve, at the latest version.
    pub object_id: ObjectID,
    /// The type of request, either latest object info or the past.
    pub request_kind: ObjectInfoRequestKind,
}

impl ObjectInfoRequest {
    pub fn past_object_info_request(object_id: ObjectID, version: SequenceNumber) -> Self {
        ObjectInfoRequest {
            object_id,
            request_kind: ObjectInfoRequestKind::PastObjectInfo(version),
        }
    }

    pub fn latest_object_info_request(
        object_id: ObjectID,
        layout: Option<ObjectFormatOptions>,
    ) -> Self {
        ObjectInfoRequest {
            object_id,
            request_kind: ObjectInfoRequestKind::LatestObjectInfo(layout),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize)]
pub struct AccountInfoResponse {
    pub object_ids: Vec<ObjectRef>,
    pub owner: SuiAddress,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectResponse {
    /// Value of the requested object in this authority
    pub object: Object,
    /// Transaction the object is locked on in this authority.
    /// None if the object is not currently locked by this authority.
    pub lock: Option<SignedTransaction>,
    /// Schema of the Move value inside this object.
    /// None if the object is a Move package, or the request did not ask for the layout
    pub layout: Option<MoveStructLayout>,
}

/// This message provides information about the latest object and its lock
/// as well as the parent certificate of the object at a specific version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectInfoResponse {
    /// The certificate that created or mutated the object at a given version.
    /// If no parent certificate was requested the latest certificate concerning
    /// this object is sent. If the parent was requested and not found a error
    /// (ParentNotfound or CertificateNotfound) will be returned.
    pub parent_certificate: Option<CertifiedTransaction>,
    /// The full reference created by the above certificate
    pub requested_object_reference: Option<ObjectRef>,

    /// The object and its current lock, returned only if we are requesting
    /// the latest state of an object.
    /// If the object does not exist this is also None.
    pub object_and_lock: Option<ObjectResponse>,
}

impl ObjectInfoResponse {
    pub fn object(&self) -> Option<&Object> {
        match &self.object_and_lock {
            Some(ObjectResponse { object, .. }) => Some(object),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize)]
pub struct TransactionInfoRequest {
    pub transaction_digest: TransactionDigest,
}

impl From<TransactionDigest> for TransactionInfoRequest {
    fn from(transaction_digest: TransactionDigest) -> Self {
        TransactionInfoRequest { transaction_digest }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransactionInfoResponse {
    // The signed transaction response to handle_transaction
    pub signed_transaction: Option<SignedTransaction>,
    // The certificate in case one is available
    pub certified_transaction: Option<CertifiedTransaction>,
    // The effects resulting from a successful execution should
    // contain ObjectRef created, mutated, deleted and events.
    pub signed_effects: Option<SignedTransactionEffects>,
}

#[derive(Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
pub enum CallResult {
    Bool(bool),
    U8(u8),
    U64(u64),
    U128(u128),
    Address(AccountAddress),
    // these are not ideal but there is no other way to deserialize
    // vectors encoded in BCS (you need a full type before this can be
    // done)
    BoolVec(Vec<bool>),
    U8Vec(Vec<u8>),
    U64Vec(Vec<u64>),
    U128Vec(Vec<u128>),
    AddrVec(Vec<AccountAddress>),
    BoolVecVec(Vec<bool>),
    U8VecVec(Vec<Vec<u8>>),
    U64VecVec(Vec<Vec<u64>>),
    U128VecVec(Vec<Vec<u128>>),
    AddrVecVec(Vec<Vec<AccountAddress>>),
}

#[derive(Eq, PartialEq, Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub enum ExecutionStatus {
    // Gas used in the success case.
    Success {
        gas_cost: GasCostSummary,
    },
    // Gas used in the failed case, and the error.
    Failure {
        gas_cost: GasCostSummary,
        #[schemars(with = "String")]
        error: Box<SuiError>,
    },
}

impl ExecutionStatus {
    pub fn new_failure(gas_used: GasCostSummary, error: SuiError) -> ExecutionStatus {
        ExecutionStatus::Failure {
            gas_cost: gas_used,
            error: Box::new(error),
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, ExecutionStatus::Success { .. })
    }

    pub fn is_err(&self) -> bool {
        matches!(self, ExecutionStatus::Failure { .. })
    }

    pub fn unwrap(self) -> GasCostSummary {
        match self {
            ExecutionStatus::Success { gas_cost: gas_used } => gas_used,
            ExecutionStatus::Failure { .. } => {
                panic!("Unable to unwrap() on {:?}", self);
            }
        }
    }

    pub fn unwrap_err(self) -> (GasCostSummary, SuiError) {
        match self {
            ExecutionStatus::Success { .. } => {
                panic!("Unable to unwrap() on {:?}", self);
            }
            ExecutionStatus::Failure {
                gas_cost: gas_used,
                error,
            } => (gas_used, *error),
        }
    }

    /// Returns the gas used from the status
    pub fn gas_cost_summary(&self) -> &GasCostSummary {
        match &self {
            ExecutionStatus::Success {
                gas_cost: gas_used, ..
            } => gas_used,
            ExecutionStatus::Failure {
                gas_cost: gas_used, ..
            } => gas_used,
        }
    }
}

/// The response from processing a transaction or a certified transaction
#[derive(Eq, PartialEq, Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TransactionEffects {
    // The status of the execution
    pub status: ExecutionStatus,
    // The object references of the shared objects used in this trasnaction. Empty if no shared objects were used.
    pub shared_objects: Vec<ObjectRef>,
    // The transaction digest
    pub transaction_digest: TransactionDigest,
    // ObjectRef and owner of new objects created.
    pub created: Vec<(ObjectRef, Owner)>,
    // ObjectRef and owner of mutated objects, including gas object.
    pub mutated: Vec<(ObjectRef, Owner)>,
    // ObjectRef and owner of objects that are unwrapped in this transaction.
    // Unwrapped objects are objects that were wrapped into other objects in the past,
    // and just got extracted out.
    pub unwrapped: Vec<(ObjectRef, Owner)>,
    // Object Refs of objects now deleted (the old refs).
    pub deleted: Vec<ObjectRef>,
    // Object refs of objects now wrapped in other objects.
    pub wrapped: Vec<ObjectRef>,
    // The updated gas object reference. Have a dedicated field for convenient access.
    // It's also included in mutated.
    pub gas_object: (ObjectRef, Owner),
    /// The events emitted during execution. Note that only successful transactions emit events
    pub events: Vec<Event>,
    /// The set of transaction digests this transaction depends on.
    pub dependencies: Vec<TransactionDigest>,
}

impl TransactionEffects {
    /// Return an iterator that iterates through both mutated and
    /// created objects.
    /// It doesn't include deleted objects.
    pub fn mutated_and_created(&self) -> impl Iterator<Item = &(ObjectRef, Owner)> {
        self.mutated.iter().chain(self.created.iter())
    }

    /// Return an iterator of mutated objects, but excluding the gas object.
    pub fn mutated_excluding_gas(&self) -> impl Iterator<Item = &(ObjectRef, Owner)> {
        self.mutated.iter().filter(|o| *o != &self.gas_object)
    }

    pub fn is_object_mutated_here(&self, obj_ref: ObjectRef) -> bool {
        // The mutated or created case
        if self.mutated_and_created().any(|(oref, _)| *oref == obj_ref) {
            return true;
        }

        // The deleted case
        if obj_ref.2 == ObjectDigest::OBJECT_DIGEST_DELETED
            && self
                .deleted
                .iter()
                .any(|(id, seq, _)| *id == obj_ref.0 && seq.increment() == obj_ref.1)
        {
            return true;
        }

        // The wrapped case
        if obj_ref.2 == ObjectDigest::OBJECT_DIGEST_WRAPPED
            && self
                .wrapped
                .iter()
                .any(|(id, seq, _)| *id == obj_ref.0 && seq.increment() == obj_ref.1)
        {
            return true;
        }
        false
    }

    pub fn to_sign_effects(
        self,
        epoch: EpochId,
        authority_name: &AuthorityName,
        secret: &dyn signature::Signer<AuthoritySignature>,
    ) -> SignedTransactionEffects {
        let signature = AuthoritySignature::new(&self, secret);

        SignedTransactionEffects {
            effects: self,
            auth_signature: AuthoritySignInfo {
                epoch,
                authority: *authority_name,
                signature,
            },
        }
    }

    pub fn to_unsigned_effects(self) -> UnsignedTransactionEffects {
        UnsignedTransactionEffects {
            effects: self,
            auth_signature: EmptySignInfo {},
        }
    }
}

impl BcsSignable for TransactionEffects {}

impl Display for TransactionEffects {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut writer = String::new();
        writeln!(writer, "Status : {:?}", self.status)?;
        if !self.created.is_empty() {
            writeln!(writer, "Created Objects:")?;
            for (obj, _) in &self.created {
                writeln!(writer, "{:?} {:?} {:?}", obj.0, obj.1, obj.2)?;
            }
        }
        if !self.mutated.is_empty() {
            writeln!(writer, "Mutated Objects:")?;
            for (obj, _) in &self.mutated {
                writeln!(writer, "{:?} {:?} {:?}", obj.0, obj.1, obj.2)?;
            }
        }
        if !self.deleted.is_empty() {
            writeln!(writer, "Deleted Objects:")?;
            for obj in &self.deleted {
                writeln!(writer, "{:?} {:?} {:?}", obj.0, obj.1, obj.2)?;
            }
        }
        write!(f, "{}", writer)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransactionEffectsEnvelope<S> {
    pub effects: TransactionEffects,
    pub auth_signature: S,
}

pub type UnsignedTransactionEffects = TransactionEffectsEnvelope<EmptySignInfo>;
pub type SignedTransactionEffects = TransactionEffectsEnvelope<AuthoritySignInfo>;

impl SignedTransactionEffects {
    pub fn digest(&self) -> [u8; 32] {
        sha3_hash(&self.effects)
    }
}

impl PartialEq for SignedTransactionEffects {
    fn eq(&self, other: &Self) -> bool {
        // We do not compare the authority signatures, because there can be multiple
        // valid signatures for the same data and signer.
        self.effects == other.effects
            && self.auth_signature.authority == other.auth_signature.authority
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputObjectKind {
    // A Move package, must be immutable.
    MovePackage(ObjectID),
    // A Move object, either immutable, or owned mutable.
    ImmOrOwnedMoveObject(ObjectRef),
    // A Move object that's shared and mutable.
    SharedMoveObject(ObjectID),
}

impl InputObjectKind {
    pub fn object_id(&self) -> ObjectID {
        match self {
            Self::MovePackage(id) => *id,
            Self::ImmOrOwnedMoveObject((id, _, _)) => *id,
            Self::SharedMoveObject(id) => *id,
        }
    }

    pub fn version(&self) -> SequenceNumber {
        match self {
            Self::MovePackage(..) => OBJECT_START_VERSION,
            Self::ImmOrOwnedMoveObject((_, version, _)) => *version,
            Self::SharedMoveObject(..) => OBJECT_START_VERSION,
        }
    }

    pub fn object_not_found_error(&self) -> SuiError {
        match *self {
            Self::MovePackage(package_id) => SuiError::DependentPackageNotFound { package_id },
            Self::ImmOrOwnedMoveObject((object_id, _, _)) => SuiError::ObjectNotFound { object_id },
            Self::SharedMoveObject(object_id) => SuiError::ObjectNotFound { object_id },
        }
    }
}
pub struct SignatureAggregator<'a> {
    committee: &'a Committee,
    weight: usize,
    used_authorities: HashSet<AuthorityName>,
    partial: CertifiedTransaction,
}

impl<'a> SignatureAggregator<'a> {
    /// Start aggregating signatures for the given value into a certificate.
    pub fn try_new(transaction: Transaction, committee: &'a Committee) -> Result<Self, SuiError> {
        transaction.check_signature()?;
        Ok(Self::new_unsafe(transaction, committee))
    }

    /// Same as try_new but we don't check the transaction.
    pub fn new_unsafe(transaction: Transaction, committee: &'a Committee) -> Self {
        Self {
            committee,
            weight: 0,
            used_authorities: HashSet::new(),
            partial: CertifiedTransaction::new(transaction),
        }
    }

    /// Try to append a signature to a (partial) certificate. Returns Some(certificate) if a quorum was reached.
    /// The resulting final certificate is guaranteed to be valid in the sense of `check` below.
    /// Returns an error if the signed value cannot be aggregated.
    pub fn append(
        &mut self,
        authority: AuthorityName,
        signature: AuthoritySignature,
    ) -> Result<Option<CertifiedTransaction>, SuiError> {
        signature.check(&self.partial.transaction.data, authority)?;
        // Check that each authority only appears once.
        fp_ensure!(
            !self.used_authorities.contains(&authority),
            SuiError::CertificateAuthorityReuse
        );
        self.used_authorities.insert(authority);
        // Update weight.
        let voting_rights = self.committee.weight(&authority);
        fp_ensure!(voting_rights > 0, SuiError::UnknownSigner);
        self.weight += voting_rights;
        // Update certificate.
        self.partial.signatures.push((authority, signature));

        if self.weight >= self.committee.quorum_threshold() {
            Ok(Some(self.partial.clone()))
        } else {
            Ok(None)
        }
    }
}

impl CertifiedTransaction {
    pub fn new(transaction: Transaction) -> CertifiedTransaction {
        CertifiedTransaction {
            transaction_digest: OnceCell::new(),
            is_checked: false,
            epoch: 0,
            transaction,
            signatures: Vec::new(),
        }
    }

    pub fn new_with_signatures(
        epoch: EpochId,
        transaction: Transaction,
        signatures: Vec<(AuthorityName, AuthoritySignature)>,
    ) -> CertifiedTransaction {
        CertifiedTransaction {
            transaction_digest: OnceCell::new(),
            is_checked: false,
            epoch,
            transaction,
            signatures,
        }
    }

    /// Get the transaction digest and write it to the cache
    pub fn digest(&self) -> &TransactionDigest {
        self.transaction_digest
            .get_or_init(|| self.transaction.digest())
    }

    /// Verify the certificate.
    pub fn check(&self, committee: &Committee) -> Result<(), SuiError> {
        // We use this flag to see if someone has checked this before
        // and therefore we can skip the check. Note that the flag has
        // to be set to true manually, and is not set by calling this
        // "check" function.
        if self.is_checked {
            return Ok(());
        }

        let mut obligation = VerificationObligation::default();
        self.add_to_verification_obligation(committee, &mut obligation)?;
        obligation.verify_all().map(|_| ())
    }

    pub fn add_to_verification_obligation(
        &self,
        committee: &Committee,
        obligation: &mut VerificationObligation,
    ) -> SuiResult<()> {
        // Check epoch
        fp_ensure!(
            self.epoch == committee.epoch(),
            SuiError::WrongEpoch {
                expected_epoch: committee.epoch()
            }
        );

        // First check the quorum is sufficient

        let mut weight = 0;
        let mut used_authorities = HashSet::new();
        for (authority, _) in self.signatures.iter() {
            // Check that each authority only appears once.
            fp_ensure!(
                !used_authorities.contains(authority),
                SuiError::CertificateAuthorityReuse
            );
            used_authorities.insert(*authority);
            // Update weight.
            let voting_rights = committee.weight(authority);
            fp_ensure!(voting_rights > 0, SuiError::UnknownSigner);
            weight += voting_rights;
        }
        fp_ensure!(
            weight >= committee.quorum_threshold(),
            SuiError::CertificateRequiresQuorum
        );

        // Add the obligation of the transaction
        self.transaction
            .add_to_verification_obligation(obligation)?;

        // Create obligations for the committee signatures

        let mut message = Vec::new();
        self.transaction.data.write(&mut message);

        let idx = obligation.messages.len();
        obligation.messages.push(message);

        for tuple in self.signatures.iter() {
            let (authority, signature) = tuple;
            // do we know, or can we build a valid public key?
            match committee.expanded_keys.get(authority) {
                Some(v) => obligation.public_keys.push(*v),
                None => {
                    let public_key = (*authority).try_into()?;
                    obligation.public_keys.push(public_key);
                }
            }

            // build a signature
            obligation.signatures.push(signature.0);

            // collect the message
            obligation.message_index.push(idx);
        }

        Ok(())
    }
}

impl Display for CertifiedTransaction {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut writer = String::new();
        writeln!(
            writer,
            "Signed Authorities : {:?}",
            self.signatures
                .iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>()
        )?;
        write!(writer, "{}", &self.transaction.data.kind)?;
        write!(f, "{}", writer)
    }
}

impl ConfirmationTransaction {
    pub fn new(certificate: CertifiedTransaction) -> Self {
        Self { certificate }
    }
}

impl BcsSignable for TransactionData {}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ConsensusOutput {
    #[serde(with = "serde_bytes")]
    pub message: Vec<u8>,
    pub sequence_number: SequenceNumber,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ConsensusSync {
    pub sequence_number: SequenceNumber,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ConsensusTransaction {
    UserTransaction(CertifiedTransaction),
    // NOTE: Other data types (e.g., for reconfiguration) go here
}

impl ConsensusTransaction {
    pub fn check(&self, committee: &Committee) -> SuiResult<()> {
        match self {
            Self::UserTransaction(certificate) => certificate.check(committee),
        }
    }
}
