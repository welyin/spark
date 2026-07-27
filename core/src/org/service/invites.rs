//! 邀请码生成与接受确认（service.ts `createOrgInvite`/`acceptOrgInvite`）。
//!
//! 本层只负责编码/解码与落库确认；`connectAndPull`（连接邀请人并反熵拉取，
//! 可捎带自签 nodeInfoClaim）属 p2p 网络层，由调用方完成后回调确认。

use crate::storage::StorageBackend;

use super::super::invite::{
    OrgInviteInviter, OrgInvitePayload, decode_org_invite_at, encode_org_invite,
};
use super::super::{OrgError, Result};
use super::{CreatedOrgInvite, InviteAcceptance, OrganizationService};

impl OrganizationService {
    /// `createOrgInvite`（service.ts:315-339）：仅 admin；邀请人节点信息归一化
    /// （peerId/addresses 至少其一，否则报"本机 P2P 节点尚未启动"）。
    pub fn create_org_invite<S: StorageBackend>(
        storage: &S,
        org_id: &str,
        current_root_id: &str,
        local_peer_id: Option<&str>,
        local_addresses: &[String],
        now_ms: i64,
    ) -> Result<CreatedOrgInvite> {
        let record = Self::require_organization(storage, org_id)?;
        Self::require_admin(&record, current_root_id)?;

        let peer_id = local_peer_id
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        let addresses: Vec<String> = local_addresses
            .iter()
            .map(|a| a.trim())
            .filter(|a| !a.is_empty())
            .map(str::to_string)
            .collect();
        if peer_id.is_none() && addresses.is_empty() {
            return Err(OrgError::NetworkUnavailable);
        }

        let payload = OrgInvitePayload::new(
            record.org_id.clone(),
            record.name.clone(),
            OrgInviteInviter {
                root_id: current_root_id.to_string(),
                peer_id,
                addresses,
            },
            now_ms,
        );
        Ok(CreatedOrgInvite {
            invite: encode_org_invite(&payload),
            org_id: record.org_id,
            org_name: record.name,
        })
    }

    /// `acceptOrgInvite` 的前半段（service.ts:345-351）：解码校验 + 拒绝自邀。
    ///
    /// 之后的 `connectAndPull`（连接邀请人并反熵拉取，可捎带自签 nodeInfoClaim）
    /// 属网络层；拉取完成后调 [`Self::check_invite_accepted`] 确认。
    pub fn prepare_accept_invite(
        code: &str,
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<OrgInvitePayload> {
        let payload = decode_org_invite_at(code, now_ms)?;
        if payload.inviter.root_id == current_root_id {
            return Err(OrgError::SelfInvite);
        }
        Ok(payload)
    }

    /// `acceptOrgInvite` 的落库确认（service.ts:365-373）：记录存在且自己为
    /// 成员才算加入成功（邀请码本身不是加入凭证，成员资格在拉取侧校验）。
    pub fn check_invite_accepted<S: StorageBackend>(
        storage: &S,
        org_id: &str,
        current_root_id: &str,
    ) -> Result<InviteAcceptance> {
        let record = Self::get_record(storage, org_id)?;
        let Some(record) = record else {
            return Err(OrgError::NotJoined);
        };
        if !record.members.iter().any(|m| m.root_id == current_root_id) {
            return Err(OrgError::NotJoined);
        }
        Ok(InviteAcceptance {
            org_id: record.org_id,
            org_name: record.name,
            member_count: record.members.len(),
        })
    }
}
