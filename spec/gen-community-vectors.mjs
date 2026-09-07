// gen-community-vectors.mjs — community-affairs C0 golden vectors 生成器（协议先行参考实现）
// 生成 code/spec/vectors/community.json。规格权威：wiki/protocol/community/（affair / org-genesis /
// org-signature / credential / read-gate / affair-metadata）；canonical JSON 规则：wiki/protocol/sync-evidence §1。
// 用法：node code/spec/gen-community-vectors.mjs   （Node >= 16，零依赖）
// 自检：全部签名 verify、全部哈希复算比对，任一失败非零退出。

import { createHash, createHmac, createPrivateKey, createPublicKey, sign, verify } from 'node:crypto';
import { writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const NOW = 1720000000000;

// ---------- canonical JSON（sync-evidence §1 normalizeObject 原样重述） ----------
function normalizeObject(value) {
  if (value === undefined) return 'undefined';
  if (value === null) return 'null';
  if (typeof value !== 'object') return JSON.stringify(value);
  const ordered = {};
  for (const k of Object.keys(value).sort()) ordered[k] = normalizeObject(value[k]);
  return JSON.stringify(ordered);
}
const sha256hex = (s) => createHash('sha256').update(typeof s === 'string' ? Buffer.from(s, 'utf8') : s).digest('hex');
const canonicalSans = (obj, key) => { const o = { ...obj }; delete o[key]; return normalizeObject(o); };

// ---------- ed25519（固定 seed 确定性密钥） ----------
function keyFromSeed(seedByte) {
  const seed = Buffer.alloc(32, seedByte);
  const pkcs8 = Buffer.concat([Buffer.from('302e020100300506032b657004220420', 'hex'), seed]);
  const priv = createPrivateKey({ key: pkcs8, format: 'der', type: 'pkcs8' });
  const pubRaw = createPublicKey(priv).export({ format: 'der', type: 'spki' }).subarray(-32);
  return {
    priv,
    publicKey: pubRaw.toString('base64'),
    identity: sha256hex(pubRaw),
    sign(payloadStr) { return sign(null, Buffer.from(payloadStr, 'utf8'), priv).toString('base64'); },
  };
}
function verifyB64(pubB64, payloadStr, sigB64) {
  const spki = Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), Buffer.from(pubB64, 'base64')]);
  return verify(null, Buffer.from(payloadStr, 'utf8'), createPublicKey({ key: spki, format: 'der', type: 'spki' }), Buffer.from(sigB64, 'base64'));
}

// ---------- base32（RFC 4648 小写去 padding，org-address §15 口径） ----------
const B32 = 'abcdefghijklmnopqrstuvwxyz234567';
function base32NoPad(buf) {
  let bits = 0, acc = 0, out = '';
  for (const b of buf) { acc = (acc << 8) | b; bits += 8; while (bits >= 5) { out += B32[(acc >> (bits - 5)) & 31]; bits -= 5; } }
  if (bits > 0) out += B32[(acc << (5 - bits)) & 31];
  return out;
}
function orgAddressFromPub(pubRaw) {
  const digest = createHash('sha256').update(pubRaw).digest();
  const checksum = createHash('sha256').update(Buffer.concat([Buffer.from('spark:org-address:', 'utf8'), digest])).digest().subarray(0, 2);
  return base32NoPad(Buffer.concat([digest, checksum]));
}

// ---------- 组织域身份派生（org-genesis §4） ----------
function orgDomainIdentity(orgRootSeedByte, domain) {
  const seed = Buffer.alloc(32, orgRootSeedByte);
  const dk = createHmac('sha512', seed).update(Buffer.concat([Buffer.from('spark:org-domain:', 'utf8'), Buffer.from(domain, 'utf8')])).digest().subarray(0, 32);
  const pkcs8 = Buffer.concat([Buffer.from('302e020100300506032b657004220420', 'hex'), dk]);
  const priv = createPrivateKey({ key: pkcs8, format: 'der', type: 'pkcs8' });
  const pubRaw = createPublicKey(priv).export({ format: 'der', type: 'spki' }).subarray(-32);
  return { publicKey: pubRaw.toString('base64'), identity: sha256hex(pubRaw), priv,
    sign(s) { return sign(null, Buffer.from(s, 'utf8'), priv).toString('base64'); } };
}

// ---------- 固定演员 ----------
const initiator = keyFromSeed(0x11);
const personA = keyFromSeed(0x21);
const personB = keyFromSeed(0x22);
const admin1 = keyFromSeed(0x41);
const admin2 = keyFromSeed(0x42);
const admin3 = keyFromSeed(0x43);
const orgRoot = keyFromSeed(0x31);           // 组织根密钥对（裸 ed25519）
const verifier = keyFromSeed(0x51);          // 验证人
const actor = (k) => ({ kind: 'person', identity: k.identity, publicKey: k.publicKey });

const fails = [];
const check = (name, cond) => { if (!cond) fails.push(name); };

// ================= affairGenesis =================
const rulesDoc = {
  engine: 'b1',
  closeConditions: [{ type: 'op-count', opType: 'content', count: 100 }],
  pubPeriod: { delayMs: 86400000 },
  participation: { contribute: { ladder: 'contributor' }, vote: { ladder: 'voter' }, combine: 'all' },
  ruleChange: { kind: 'delayed-veto', delayMs: 259200000, vetoThreshold: { count: 3 } },
  exec: null,
};
const genesis = {
  affairV: 1, type: 'forum', title: '第二届业委会选举', summary: '阳光小区业委会换届选举',
  tags: ['region:110105', 'hoa'],
  initiator: actor(initiator),
  rules: rulesDoc,
  initialVoters: [initiator.identity],
  refs: [{ target: sha256hex('previous-election-affair'), rel: 'inherit' }],
  createdAt: NOW,
};
const genesisPayload = canonicalSans(genesis, 'sig');
genesis.sig = initiator.sign(genesisPayload);
const affairId = sha256hex(genesisPayload);
check('affairGenesis.sig', verifyB64(initiator.publicKey, genesisPayload, genesis.sig));
check('affairGenesis.idShape', /^[0-9a-f]{64}$/.test(affairId));

// ================= opChain =================
function makeOp(prevOpHash, opType, payload, key, declaredAt) {
  const op = { opV: 1, affairId, prevOpHash, opType, payload, actor: actor(key), declaredAt };
  const p = canonicalSans(op, 'sig');
  op.sig = key.sign(p);
  return { op, payloadStr: p, opHash: sha256hex(normalizeObject(op)) };
}
const op1 = makeOp(affairId, 'content', { kind: 'post', text: '提名候选人公示' }, initiator, NOW + 1000);
const op2 = makeOp(op1.opHash, 'content', { kind: 'post', text: '附议' }, personA, NOW + 2000);
const op3 = makeOp(op1.opHash, 'ref', { target: sha256hex('related-affair'), rel: 'related' }, personB, NOW + 3000);
for (const [n, o] of [['op1', op1], ['op2', op2], ['op3', op3]])
  check(`opChain.${n}.sig`, verifyB64(o.op.actor.publicKey, o.payloadStr, o.op.sig));
check('opChain.dag-branch', op2.op.prevOpHash === op3.op.prevOpHash); // 合法 DAG 分支
const sortOrder = [op1, op2, op3].map(o => o.opHash).sort();

// ================= ruleMechanisms =================
const mechVote = { kind: 'vote', voterSet: 'ladder:voters', threshold: { num: 1, den: 2 }, quorum: { num: 1, den: 2 }, snapshot: 'required' };
const mechMultisig = { kind: 'multisig', m: 2, n: 3, signers: [admin1.identity, admin2.identity, admin3.identity] };
const mechDelayedVeto = { kind: 'delayed-veto', delayMs: 259200000, vetoThreshold: { count: 3 } };

// ================= snapshot / rosterCommit =================
const roster = [initiator, personA, personB].map(k => ({ identity: k.identity })).sort((a, b) => a.identity < b.identity ? -1 : 1);
const rosterHash = sha256hex(normalizeObject(roster));
const memberSet = [admin1, admin2, admin3].map(k => ({ identity: k.identity, role: 'admin' })).sort((a, b) => a.identity < b.identity ? -1 : 1);
const memberSetHash = sha256hex(normalizeObject(memberSet));

// ================= orgGenesis =================
const orgAddress = orgAddressFromPub(Buffer.from(orgRoot.publicKey, 'base64'));
const orgGenesisRec = {
  genesisV: 1, name: '阳光小区', description: '阳光小区共同体域',
  domainType: 'community',
  rootPublicKey: orgRoot.publicKey, orgAddress,
  signingPolicy: { kind: 'any-admin' },
  transition: { kind: 'delayed-veto', delayMs: 259200000, vetoThreshold: { count: 1 }, whenMembersExceed: 1 },
  createdBy: initiator.identity, createdAt: NOW,
};
const orgGenesisPayload = canonicalSans(orgGenesisRec, 'sig');
orgGenesisRec.sig = orgRoot.sign(orgGenesisPayload);
const newOrgId = 'org_' + sha256hex(orgGenesisPayload);
check('orgGenesis.sig', verifyB64(orgRoot.publicKey, orgGenesisPayload, orgGenesisRec.sig));
check('orgGenesis.addrBind', orgGenesisRec.orgAddress === orgAddress);
check('orgGenesis.addrLen', orgAddress.length === 55);
check('orgGenesis.idShape', /^org_[0-9a-f]{64}$/.test(newOrgId));

// ================= orgDomainIdentity =================
const orgMemberId = orgDomainIdentity(0x31, `community:${newOrgId}`);
const orgAffairId = orgDomainIdentity(0x31, `affair:${affairId}`);
check('orgDomainIdentity.distinct', orgMemberId.identity !== orgAffairId.identity);

// ================= policyChain =================
const policyHash0 = sha256hex(orgGenesisPayload);
const sigSetForPolicy = (subject, signers) => {
  const base = { sigSetV: 1, orgId: newOrgId, subject, policyHash: policyHash0, memberSetHash, anchorRoot: sha256hex('anchor-root'), anchorTs: NOW, signedAt: NOW };
  const cp = normalizeObject(base);
  return {
    sigSetV: 1, orgId: newOrgId, subject, policyHash: policyHash0,
    roster: { memberSetHash, anchor: { orgId: newOrgId, anchorRoot: base.anchorRoot, ts: NOW }, snapshot: memberSet },
    signedAt: NOW,
    signatures: signers.map(k => ({ signer: k.identity, publicKey: k.publicKey, sig: k.sign(cp) })),
    _componentPayload: cp,
  };
};
const policyRev = {
  policyV: 1, orgId: newOrgId, seq: 1, prevPolicyHash: policyHash0,
  signingPolicy: { kind: 'm-of-n', m: 2, n: 3 }, updatedAt: NOW + 10000,
};
const policyRevPayload = canonicalSans(policyRev, 'sigSet');
policyRev.sigSet = sigSetForPolicy(sha256hex(policyRevPayload), [admin1, admin2]);
delete policyRev.sigSet._componentPayload;
const policyHash1 = sha256hex(policyRevPayload);

// ================= orgSigSet =================
const subject = sha256hex('subject-under-signing');
const sigSetAny = sigSetForPolicy(subject, [admin1]);
const sigSetMofN = sigSetForPolicy(subject, [admin1, admin2]);
const cp = sigSetAny._componentPayload;
for (const s of sigSetAny.signatures) check('orgSigSet.anyAdmin.sig', verifyB64(s.publicKey, cp, s.sig));
for (const s of sigSetMofN.signatures) check('orgSigSet.mOfN.sig', verifyB64(s.publicKey, cp, s.sig));
const tamperedCp = normalizeObject({ sigSetV: 1, orgId: newOrgId, subject: sha256hex('forged'), policyHash: policyHash0, memberSetHash, anchorRoot: sha256hex('anchor-root'), anchorTs: NOW, signedAt: NOW });
check('orgSigSet.tamper.subjectRejected', !verifyB64(admin1.publicKey, tamperedCp, sigSetAny.signatures[0].sig));
delete sigSetAny._componentPayload; delete sigSetMofN._componentPayload;

// ================= credential =================
const holderOrg = orgMemberId; // 持有组织在该共同体的域身份
const cred = {
  credV: 1, credType: 'household-owner',
  issuer: { identity: verifier.identity, publicKey: verifier.publicKey },
  holder: { kind: 'org', identity: holderOrg.identity, publicKey: holderOrg.publicKey },
  subjectDomain: newOrgId,
  claims: { household: '3-502' },
  method: 'plugin:hoa-verify:manual-property-cert',
  linkRef: null, issuedAt: NOW,
};
const credPayload = canonicalSans(cred, 'sig');
cred.sig = verifier.sign(credPayload);
const credId = sha256hex(credPayload);
check('credential.issue.sig', verifyB64(verifier.publicKey, credPayload, cred.sig));

// 注销链（3 条）
function revEntry(seq, prevHash, credIdX) {
  const e = { revV: 1, issuer: verifier.identity, seq, prevHash, credId: credIdX, revokedAt: NOW + seq * 1000, reason: null };
  const p = canonicalSans(e, 'sig');
  e.sig = verifier.sign(p);
  return { entry: e, payloadStr: p, entryHash: sha256hex(p) };
}
const rev1 = revEntry(1, null, sha256hex('cred-to-revoke-1'));
const rev2 = revEntry(2, rev1.entryHash, sha256hex('cred-to-revoke-2'));
const rev3 = revEntry(3, rev2.entryHash, sha256hex('cred-to-revoke-3'));
for (const [n, r] of [['rev1', rev1], ['rev2', rev2], ['rev3', rev3]])
  check(`credential.revokeChain.${n}`, verifyB64(verifier.publicKey, r.payloadStr, r.entry.sig));
const revHead = { revHeadV: 1, issuer: verifier.identity, headSeq: 3, headHash: rev3.entryHash, asOf: NOW + 5000 };
const revHeadPayload = canonicalSans(revHead, 'sig');
revHead.sig = verifier.sign(revHeadPayload);
check('credential.revHead.sig', verifyB64(verifier.publicKey, revHeadPayload, revHead.sig));

// ================= trustDecl =================
const trustDecl = {
  trustV: 1, orgId: newOrgId,
  verifiers: [{ identity: verifier.identity, publicKey: verifier.publicKey, credTypes: ['household-owner', 'resident'], methods: ['plugin:hoa-verify:*'] }],
  effectiveFrom: NOW, seq: 1, updatedAt: NOW,
};
const trustPayload = canonicalSans(trustDecl, 'sigSet');
trustDecl.sigSet = sigSetForPolicy(sha256hex(trustPayload), [admin1]);
const trustComponent = trustDecl.sigSet._componentPayload;
check('trustDecl.sigSet', verifyB64(admin1.publicKey, trustComponent, trustDecl.sigSet.signatures[0].sig));
delete trustDecl.sigSet._componentPayload;
const trustHash = sha256hex(trustPayload);

// ================= samePersonLink =================
const link = {
  linkV: 1, statement: 'same-person',
  members: [{ holderIdentity: holderOrg.identity, credId }, { holderIdentity: orgAffairId.identity, credId: sha256hex('second-cred') }],
  issuer: { identity: verifier.identity, publicKey: verifier.publicKey },
  subjectDomain: newOrgId, issuedAt: NOW,
};
const linkPayload = canonicalSans(link, 'sig');
link.sig = verifier.sign(linkPayload);
const linkId = sha256hex(linkPayload);
check('samePersonLink.sig', verifyB64(verifier.publicKey, linkPayload, link.sig));
check('samePersonLink.minMembers', link.members.length >= 2);

// ================= readGate =================
const requestId = 'req-' + sha256hex('nonce').slice(0, 16);
const holderProofPayload = normalizeObject({ credId, requestId, orgId: newOrgId, collection: 'hoa:ledger@v1.0.0', presentedAt: NOW });
const holderProof = { credId, sig: holderOrg.sign(holderProofPayload) };
check('readGate.holderProof', verifyB64(holderOrg.publicKey, holderProofPayload, holderProof.sig));
const declWithReadPolicy = {
  name: 'hoa:ledger', version: '1.0.0', space: 'org', accounts: 'data-accounts', devices: 'all',
  confidentiality: 'filtered', merge: 'lww-record',
  readPolicy: { kind: 'credential', credTypes: ['household-owner', 'resident'], verifierDomain: newOrgId, policyRef: null },
  declaredBy: initiator.identity, declaredAt: NOW, ts: NOW,
};

// ================= metaAnnounce =================
const announce = {
  metaV: 1, affairId, title: genesis.title, summary: genesis.summary, tags: genesis.tags,
  region: '110105', metaSeq: 0, basisOpHash: affairId,
  contents: [{ kind: 'git', ref: 'spark-git://' + sha256hex('repo').slice(0, 32) }],
  updatedAt: NOW,
};
const metaEnvelope = { version: '1', type: 'affair-meta', domain: 'affair', id: affairId, payload: announce, timestamp: NOW };

// ================= 输出 =================
const out = {
  _comment: 'community-affairs C0 golden vectors（wiki/protocol/community/ 协议先行）。生成器：code/spec/gen-community-vectors.mjs（Node 参考实现自产，含签名/哈希自检）；C1/C3 实现落地后以 core/examples 生成器复核固化。消费（占位）：core/tests/community_vectors.rs。',
  meta: {
    constants: { nowMs: NOW, freshnessWindowMs: 600000, defaultPubPeriodMs: 86400000 },
    actors: {
      initiator: { publicKey: initiator.publicKey, identity: initiator.identity },
      personA: { publicKey: personA.publicKey, identity: personA.identity },
      personB: { publicKey: personB.publicKey, identity: personB.identity },
      admin1: { publicKey: admin1.publicKey, identity: admin1.identity },
      admin2: { publicKey: admin2.publicKey, identity: admin2.identity },
      admin3: { publicKey: admin3.publicKey, identity: admin3.identity },
      orgRoot: { publicKey: orgRoot.publicKey, identity: orgRoot.identity },
      verifier: { publicKey: verifier.publicKey, identity: verifier.identity },
    },
    placeholders: {
      desc: '依赖实现的 case 组（登记于规格文档末节，C1/C2/C3/C6/C10 落地时以 core/examples 提取复核）：staticCheck / resolutionReplay / ladderDerive（affair §12）；cycleCheck / memberKindEnforce（org-genesis §7）；legacyDegraded（org-signature §6）；credential.trustTimeline（credential §7）；readGate.verifyChain（read-gate §6）；metaArbitrate / metaBasisVerify（affair-metadata §7）',
    },
  },
  affairGenesis: {
    desc: '固定创世输入 → canonical 签名载荷逐字节 + affairId + sig 固定值；篡改任一字段 affairId/验签必败（消费侧断言）',
    input: { seedByte: 17, createdAt: NOW },
    expect: { payload: genesisPayload, affairId, record: genesis },
  },
  opChain: {
    desc: '3 条固定操作（含合法 DAG 分支：op2/op3 同指 op1）→ opHash 链逐字节；首条 prevOpHash = affairId；sortOrder = opHash 字典序（§8 排序键）',
    expect: {
      ops: [op1, op2, op3].map(o => ({ payload: o.payloadStr, opHash: o.opHash, entry: o.op })),
      sortOrder,
    },
  },
  ruleMechanisms: {
    desc: '§5.3 三形态机制文档 canonical 逐字节',
    expect: { vote: normalizeObject(mechVote), multisig: normalizeObject(mechMultisig), delayedVeto: normalizeObject(mechDelayedVeto) },
  },
  snapshot: {
    desc: '固定阶梯名册 → rosterHash；固定组织名册 → memberSetHash（均按 identity 字典序）',
    expect: { roster, rosterHash, memberSet, memberSetHash },
  },
  orgGenesis: {
    desc: '固定创世策略记录 → canonical 载荷逐字节 + 新 orgId（org_+64hex）+ sig 固定值；orgAddress 互绑复算（base32(sha256(pub)‖checksum) = 55 字符）',
    input: { orgRootSeedByte: 49, createdAt: NOW },
    expect: { payload: orgGenesisPayload, orgId: newOrgId, orgAddress, record: orgGenesisRec, policyHash0 },
  },
  orgDomainIdentity: {
    desc: 'org-genesis §4：固定组织根私钥 + 域串 → HMAC-SHA512 派生域公钥/身份 id 固定值；两域串派生不同身份（跨域不可关联）',
    expect: {
      community: { domain: `community:${newOrgId}`, ...(({ publicKey, identity }) => ({ publicKey, identity }))(orgMemberId) },
      affair: { domain: `affair:${affairId}`, ...(({ publicKey, identity }) => ({ publicKey, identity }))(orgAffairId) },
    },
  },
  policyChain: {
    desc: '创世 policyHash0 + 一条修订（any-admin → m-of-n 2/3，按修订前策略双签）→ policyHash1 链逐字节',
    expect: { policyHash0, revisionPayload: policyRevPayload, policyHash1, revision: policyRev },
  },
  'orgSigSet.anyAdmin': {
    desc: 'any-admin：单管理员分量签名包；分量载荷 8 键固定集逐字节；验证通过',
    expect: { componentPayload: cp, sigSet: sigSetAny },
  },
  'orgSigSet.mOfN': {
    desc: 'm-of-n 2/3：两有效分量通过；单分量不足阈值必败、非 admin signer 必败、重复 signer 只计一次（消费侧断言）',
    expect: { componentPayload: cp, sigSet: sigSetMofN },
  },
  'orgSigSet.tamper': {
    desc: '篡改 subject/policyHash/memberSetHash/signedAt 后分量载荷变化 → 原签名验签必败（示例：forged subject）',
    expect: { forgedSubjectPayload: tamperedCp, validSigOnForged: false },
  },
  'credential.issue': {
    desc: '固定凭证 → canonical 载荷逐字节 + credId + sig 固定值；篡改字段必败',
    expect: { payload: credPayload, credId, credential: cred },
  },
  'credential.revokeChain': {
    desc: '3 条注销条目 → entryHash/prevHash 链逐字节；断链（删中间条目）必败',
    expect: { entries: [rev1, rev2, rev3].map(r => ({ payload: r.payloadStr, entryHash: r.entryHash, entry: r.entry })) },
  },
  'credential.revHead': {
    desc: '注销列表头承诺 → 签名载荷逐字节 + sig 固定值',
    expect: { payload: revHeadPayload, head: revHead },
  },
  trustDecl: {
    desc: '验证人信任声明 → canonical 逐字节 + trustHash；sigSet（any-admin）验证通过；无效 sigSet 拒绝合入（消费侧断言）',
    expect: { payload: trustPayload, trustHash, record: trustDecl },
  },
  samePersonLink: {
    desc: '同人关联声明（opt-in）→ linkId + sig 固定值；members<2 拒绝（消费侧断言）',
    expect: { payload: linkPayload, linkId, record: link },
  },
  'readGate.envelope': {
    desc: 'holderProof：载荷 = canonical({credId,requestId,orgId,collection,presentedAt}) 逐字节 + holder 私钥签名固定值',
    expect: { requestId, holderProofPayload, holderProof },
  },
  'readGate.declExt': {
    desc: '集合声明追加 readPolicy 的 canonical 往返（既有字段一字节不变）',
    expect: { canonical: normalizeObject(declWithReadPolicy), record: declWithReadPolicy },
  },
  metaAnnounce: {
    desc: '元数据公告 canonical 逐字节 + 信封形态（version/type/domain/id 固定值；信封不强制签名，可验证性由 basisOpHash 承担）',
    expect: { announceCanonical: normalizeObject(announce), envelope: metaEnvelope },
  },
};

if (fails.length) { console.error('SELF-CHECK FAILED:', fails); process.exit(1); }
writeFileSync(join(HERE, 'vectors', 'community.json'), JSON.stringify(out, null, 2) + '\n');
console.log(`OK: community.json written, ${Object.keys(out).length - 2} case groups, self-checks passed.`);
