/**
 * 联系人名首字母分组工具（设计 ui-contacts §2.3）。
 *
 * TODO(mock): 汉字首字母靠下面这张小型常用字映射表，覆盖常见姓氏与名字用字
 * 但并不全（生僻字会落入 '#' 组）；后续按设计 §10 换成 pinyin 库或
 * Intl.Collator 边界判定，届时本文件整体替换、调用方不变。
 */

// 每个字母一串常用汉字（姓氏 + 高频名字用字），命中即归入该字母组
const LETTER_CHARS: Record<string, string> = {
  A: '阿啊艾爱安岸按昂敖澳',
  B: '八巴白百柏班包宝保鲍贝倍本毕边卞表冰兵丙波博薄卜不布步',
  C: '才采蔡苍操曹岑柴昌常超朝车陈晨成程池迟赤冲初楚储川传春纯慈聪从丛崔村',
  D: '大代戴丹党刀德灯邓狄笛地丁东冬董动窦杜段端多',
  E: '儿尔耳二恩鄂',
  F: '发凡樊范方房菲飞费丰风封峰冯凤符福付傅富',
  G: '干甘刚高哥歌革葛根耿工公宫巩古谷顾关官管光桂郭国果过',
  H: '哈海韩寒杭郝好何和贺赫黑洪侯厚胡花华滑怀欢黄回惠慧霍',
  J: '机吉季纪计家加佳甲贾坚简江姜蒋焦杰洁金锦靳京经荆井景静敬九久居菊巨娟军君',
  K: '卡开凯康柯可克空孔寇库快宽匡奎坤',
  L: '来赖兰蓝郎老乐雷冷黎李理力立丽利莉连莲良梁廖林凌玲灵刘柳龙隆娄卢鲁陆路鹿罗骆吕绿伦',
  M: '麻马麦满毛茅梅美门蒙孟米宓苗妙闵明名莫墨牟木牧慕穆',
  N: '那南楠念聂宁牛农努女暖',
  O: '欧偶区',
  P: '潘盘庞裴彭皮平萍蒲浦朴',
  Q: '七齐祁奇启钱强乔巧秦青卿清庆丘邱秋求裘曲屈瞿权全泉阙雀',
  R: '冉饶任仁戎荣容柔如阮瑞润若',
  S: '萨赛桑沙山单商尚邵沈盛施石时史舒帅双水司松宋苏宿素孙索',
  T: '台谭汤唐陶腾滕田铁佟通涂屠谈檀涛天婷',
  W: '万汪王危韦魏温文闻翁巫吴伍武邬伟维卫未',
  X: '夕奚习席夏冼项肖谢解辛邢星熊修徐许薛荀雪霞晓小欣新秀英雄',
  Y: '严阎颜晏杨姚叶伊易殷尹应雍尤于余俞虞郁喻袁岳云燕羊阳洋瑶耀勇友雨玉元园媛月越',
  Z: '臧曾翟詹张章赵珍真郑钟周朱祝庄卓宗邹祖左查梓子紫宗'
};

const CHAR_TO_LETTER = new Map<string, string>();
for (const [letter, chars] of Object.entries(LETTER_CHARS)) {
  for (const char of chars) {
    CHAR_TO_LETTER.set(char, letter);
  }
}

const collator = new Intl.Collator('zh-Hans-CN', { sensitivity: 'base' });

/** 名字分组字母：英文字母直接取大写，汉字查映射表，其余（数字/特殊字符/生僻字）归 '#' */
export function firstLetter(name: string): string {
  const first = [...name.trim()][0];
  if (!first) {
    return '#';
  }
  if (/^[a-z]$/i.test(first)) {
    return first.toUpperCase();
  }
  return CHAR_TO_LETTER.get(first) ?? '#';
}

/** 组内全序：拼音/字母序（Intl.Collator zh 按拼音排汉字），'# '组交给调用方排最后 */
export function compareNames(a: string, b: string): number {
  return collator.compare(a, b);
}

/** 分组键排序：A..Z 在前，'#' 恒最后 */
export function compareLetters(a: string, b: string): number {
  if (a === '#') {
    return b === '#' ? 0 : 1;
  }
  if (b === '#') {
    return -1;
  }
  return a.localeCompare(b);
}
