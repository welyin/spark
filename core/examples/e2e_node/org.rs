//! 组织命令：org-create/org-add-member/org-send-invite/org-respond-invite/
//! org-invite-records/org-list/org-view/org-update-info/org-update-my-identity。

use serde_json::Value;
use spark_core::kernel::Kernel;
use spark_core::org::OrganizationNodeInfo;
use spark_core::org::service::{CreateOrganizationInput, OrgIdentityPatch};

use crate::dispatch::{Params, to_json};

/// `org-create`：name 必填，description/avatar/basePluginDomain 可省。
pub fn create(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    to_json(kernel.create_org(CreateOrganizationInput {
        name: params.need_str("name")?.to_string(),
        description: params.opt_str("description").map(ToString::to_string),
        avatar: params.opt_str("avatar").map(ToString::to_string),
        base_plugin_domain: params
            .opt_str("basePluginDomain")
            .map(ToString::to_string),
    }))
}

/// `org-add-member`：预录成员（nodeInfo 可带 peerId/addresses，供推送直连寻址）。
pub fn add_member(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let org_id = params.need_str("orgId")?;
    let root_id = params.need_str("rootId")?;
    let node_info = params.opt_value("nodeInfo").map(|v| OrganizationNodeInfo {
        peer_id: v
            .get("peerId")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        addresses: v
            .get("addresses")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    });
    to_json(kernel.org_add_member(org_id, root_id, node_info.as_ref()))
}

/// `org-send-invite`：经 DM 发出组织邀请（仅 admin；投递尽力而为）。
pub fn send_invite(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let org_id = params.need_str("orgId")?;
    let target_root_id = params.need_str("targetRootId")?;
    let addresses = params.opt_strings("targetAddresses").unwrap_or_default();
    to_json(kernel.org_send_invite(
        org_id,
        target_root_id,
        params.opt_str("targetPeerId"),
        &addresses,
        params.opt_str("targetNickname"),
    ))
}

/// `org-respond-invite`：回应入站邀请（accept=true 走加入编排；幂等）。
pub fn respond_invite(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let invite_id = params.need_str("inviteId")?;
    let accept = params.opt_bool("accept").unwrap_or(false);
    to_json(kernel.org_respond_invite(invite_id, accept))
}

/// `org-invite-records`：指定组织的出/入站邀请记录。
pub fn invite_records(kernel: &Kernel, params: &Params) -> Result<Value, String> {
    to_json(kernel.org_invite_records(params.need_str("orgId")?))
}

/// `org-list`：我的组织列表。
pub fn list(kernel: &Kernel) -> Result<Value, String> {
    to_json(kernel.list_orgs())
}

/// `org-view`：按 orgId 取单个组织视图（不存在 → null）。
pub fn view(kernel: &Kernel, params: &Params) -> Result<Value, String> {
    let org_id = params.need_str("orgId")?;
    let orgs = kernel.list_orgs().map_err(|e| e.to_string())?;
    let found = orgs.into_iter().find(|o| o.record.org_id == org_id);
    serde_json::to_value(found).map_err(|e| e.to_string())
}

/// `org-update-info`：name/description/avatar 三态（缺省不变；avatar "" = 清除 logo）。
pub fn update_info(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let org_id = params.need_str("orgId")?;
    to_json(kernel.org_update_info(
        org_id,
        params.tri_str("name"),
        params.tri_str("description"),
        params.tri_str("avatar"),
    ))
}

/// `org-update-my-identity`：改自己的组织内身份（avatar 三态："" 清除）。
pub fn update_my_identity(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let org_id = params.need_str("orgId")?;
    let patch = OrgIdentityPatch {
        nickname: params.opt_str("nickname").map(ToString::to_string),
        avatar: params
            .tri_str("avatar")
            .map(|value| (!value.is_empty()).then(|| value.to_string())),
        gender: params.tri_str("gender").map(ToString::to_string),
        region: params.tri_str("region").map(ToString::to_string),
        signature: params.tri_str("signature").map(ToString::to_string),
        use_personal_identity: params.opt_bool("usePersonalIdentity"),
    };
    to_json(kernel.org_update_my_identity(org_id, &patch))
}
