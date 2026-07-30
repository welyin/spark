//! 组织服务层单测：按域拆分——创建/删除在 `create`，成员与视图在 `members`，
//! 邀请码在 `invite`，入站落库（快照/nodeInfoClaim/recovery 视图）在 `incoming`，
//! 网关与公开标志在 `settings`。本文件放各域共用的夹具（固定时间、测试
//! 助记词、rootId 生成、建组快捷函数）。

#[path = "service/create.rs"]
mod create;
#[path = "service/incoming.rs"]
mod incoming;
#[path = "service/invite.rs"]
mod invite;
#[path = "service/members.rs"]
mod members;
#[path = "service/settings.rs"]
mod settings;

use spark_core::identity::{derive_root_identity, parse_mnemonic};
use spark_core::org::types::OrganizationRecord;
use spark_core::storage::MemoryStorage;

use spark_core::org::service::{CreateOrganizationInput, OrganizationService};

const NOW: i64 = 1_720_000_000_000;
const MNEMONIC: &str = "与 祝 产 鸡 永 烂 施 师 蓝 荷 有 邓 朗 防 管 李 原 芳 饿 万 措 走 腰 旅";
const MNEMONIC2: &str = "legal winner thank year wave sausage worth useful legal winner thank year wave sausage worth useful legal will";

fn root_id_of(mnemonic: &str) -> String {
    let parsed = parse_mnemonic(mnemonic).unwrap();
    derive_root_identity(&parsed.seed).id()
}

fn rid(ch: char) -> String {
    ch.to_string().repeat(64)
}

fn input() -> CreateOrganizationInput {
    CreateOrganizationInput {
        name: "  星火   组织 ".to_string(),
        description: Some(" 描述 ".to_string()),
        avatar: None,
        base_plugin_domain: Some(" plugin:chat ".to_string()),
    }
}

fn setup_org(storage: &mut MemoryStorage) -> (String, OrganizationRecord) {
    let admin = root_id_of(MNEMONIC);
    let record = OrganizationService::create_organization(storage, &input(), &admin, NOW).unwrap();
    (admin, record)
}
