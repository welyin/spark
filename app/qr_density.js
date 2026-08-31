const QRCode = require('qrcode');

const payload =
  '{"v":2,"kdf":"scrypt","salt":"ABEiM0RVZneImaq7zN3u/w==","iv":"AQIDBAUGBwgJCgsM","data":"' +
  'Q8rT3hJkLmNpQvXzYw0eKbFdGsHaUiRoPlSmTnUoVpWqXrYsZtAuBvCwDxEyFzG0H1I2J3K4L5M6N7O8P9QaRbScTdUeVfWgXhYiZjAkBlCmDnEoFpGqHrIsJtKuLvMwNxOyPzQ0R1S2T3U4V5W6X7Y8Z9aAbBcCdDeEfFgGhHiIiJjKkLlMmNnOoPpQqRrSsTtUuVvWwXxYyZz01' +
  '","authTag":"28z1wea1Iae6ssdrbWYuRg==","rootId":"690b0ff9ccb66987644883c452af35f81ef6c3be5a3b44702d8c199b7fb7e0de","createdAt":1750000000,"updatedAt":0}';

console.log('payload chars:', payload.length);

for (const ec of ['L', 'M', 'Q', 'H']) {
  const qr = QRCode.create(payload, { errorCorrectionLevel: ec });
  const md = qr.modules.size;
  console.log(`ec=${ec}  QR v${qr.version}  modules=${md}x${md}`);
}

console.log('\n渲染每模块像素（当前 width=qrWidth*4）：');
const mdM = QRCode.create(payload, { errorCorrectionLevel: 'M' }).modules.size;
const mdL = QRCode.create(payload, { errorCorrectionLevel: 'L' }).modules.size;
for (const w of [240, 320, 360]) {
  const render = w * 4;
  console.log(`display=${w}px render=${render}px  ec=M:${(render / mdM).toFixed(1)}px/module  ec=L:${(render / mdL).toFixed(1)}px/module`);
}
