//! v0.3 管理页：静态 HTML 直接编译进二进制，零构建、无 CDN、无运行时前端依赖。
//! 只用一个极小的 HTTP/1.1 服务，不引入 Web 框架，方便在无 GUI 的 NAS 上单文件部署。

use crate::daemon::{DaemonState, PeerInfo};
use crate::trust::{TrustEvent, TrustedPeer};
use crate::queue::now_secs;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

/// 管理页本体。所有颜色/字体走 :root 变量；内容默认全部可见（关掉脚本也能读到信息）。
/// 页面只显示后端真实返回的数据，不摆任何编造的数字、徽标或装饰性状态点。
const PAGE: &str = r##"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>通传 · 管理</title>
<style>
:root{
  --bg:#f6f5f2; --surface:#fff; --ink:#1d1c1a; --muted:#6d6a64;
  --line:#dedbd4; --accent:#1f6f5c; --accent-soft:#e8f1ed; --warn:#9a5b23;
  --font:system-ui,-apple-system,"Segoe UI","Microsoft YaHei",sans-serif;
  --mono:ui-monospace,"Cascadia Mono",Consolas,monospace;
  --gap:20px; --radius:10px;
}
html,body{overflow-x:clip;margin:0;background:var(--bg);color:var(--ink);font:16px/1.55 var(--font)}
body{padding:32px 20px 64px}
.wrap{max-width:920px;margin:0 auto}
header{display:flex;flex-wrap:wrap;align-items:baseline;gap:12px;border-bottom:1px solid var(--line);padding-bottom:18px;margin-bottom:28px}
h1{font-size:26px;font-weight:650;margin:0;letter-spacing:-.01em}
.version{color:var(--muted);font-size:14px}
section{
  background:var(--surface);border:1px solid var(--line);border-radius:var(--radius);padding:20px 22px;margin-bottom:var(--gap)
}
.mt{margin-top:12px}
h2{font-size:16px;font-weight:600;margin:0 0 16px}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(0,1fr));gap:14px}
.item{min-width:0}
.label{display:block;color:var(--muted);font-size:13px;margin-bottom:4px}
.value{font-size:17px;overflow-wrap:break-word;min-width:0}
table{width:100%;border-collapse:collapse;font-size:14px}
th,td{text-align:left;padding:9px 8px;border-bottom:1px solid var(--line);overflow-wrap:break-word}
th{color:var(--muted);font-weight:500;font-size:13px}
pre{font:13px/1.6 var(--mono);white-space:pre-wrap;word-break:break-word;max-height:280px;overflow:auto;margin:0;color:var(--ink)}
button,input{font:inherit;color:var(--ink)}
select{font:inherit;color:var(--ink);border:1px solid var(--line);border-radius:8px;padding:8px 10px;background:var(--surface);max-width:100%}
button{background:var(--accent);color:#fff;border:0;border-radius:8px;padding:9px 18px;cursor:pointer}
button:hover{background:#185a4b}
input[type=text]{border:1px solid var(--line);border-radius:8px;padding:8px 10px;width:min(320px,100%)}
button:focus-visible,input:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
.row{display:flex;flex-wrap:wrap;gap:12px;align-items:end}
.hint{color:var(--muted);font-size:13px;margin-top:10px}
.empty{color:var(--muted);font-size:14px}
@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}}
</style>
</head>
<body>
<div class="wrap">
<header><h1>通传</h1><span class="version" id="ver">加载中</span></header>

<section>
  <h2>概览</h2>
  <div class="grid">
    <div class="item"><span class="label">设备名称</span><div class="value" id="alias">—</div></div>
    <div class="item"><span class="label">传输监听</span><div class="value" id="listen">—</div></div>
    <div class="item"><span class="label">运行时长</span><div class="value" id="uptime">—</div></div>
    <div class="item"><span class="label">证书指纹</span><div class="value" id="fp">—</div></div>
  </div>
  <p class="hint">当前传输：<span id="current">空闲</span></p>
  <table><thead><tr><th>已发现设备</th><th>地址</th><th>最近可见</th></tr></thead><tbody id="peers"><tr><td colspan="3" class="empty">暂无</td></tr></tbody></table>
</section>

<section>
  <h2>发送</h2>
  <div class="row">
    <div class="item" style="flex:1"><span class="label">目标设备</span><select id="sendTarget" style="width:100%"><option value="">加载中</option></select></div>
    <button id="sendReload">刷新设备</button>
  </div>
  <div class="row mt">
    <div class="item" style="flex:1"><span class="label">要发送的目录 / 文件</span><input type="text" id="sendPath" style="width:100%" placeholder="例如 C:\\照片"></div>
    <button id="sendBrowse">浏览…</button>
    <button id="sendGo">发送</button>
  </div>
  <div class="row mt">
    <div class="item"><span class="label">配对码（可选）</span><input type="text" id="sendPair" placeholder="对端要求配对码时填，仅本次使用"></div>
    <div class="item"><span class="label">强制重传</span>
      <label><input type="checkbox" id="sendForce"> 忽略接收端已有文件，本次全部重传（同名旧文件改名让位，不删除）</label>
    </div>
  </div>
  <div id="browseMask" style="display:none;position:fixed;inset:0;background:rgba(0,0,0,.35);z-index:50"></div>
  <div id="browse" style="display:none;position:fixed;left:50%;top:50%;transform:translate(-50%,-50%);width:min(560px,92vw);max-height:82vh;overflow:auto;background:var(--surface);border:1px solid var(--line);border-radius:12px;padding:18px 20px;box-shadow:0 18px 50px rgba(0,0,0,.25);z-index:51">
    <div style="display:flex;align-items:center;gap:8px;margin-bottom:10px">
      <strong style="font-size:15px">选择目录</strong>
      <button id="browseClose" style="margin-left:auto">关闭</button>
    </div>
    <div class="hint" id="browsePath" style="margin-bottom:8px">—</div>
    <div style="display:flex;gap:8px;margin-bottom:8px">
      <button id="browseUp">上级</button>
      <button id="browsePick">选这个目录</button>
    </div>
    <div id="browseList" style="max-height:52vh;overflow:auto"></div>
  </div>
  <p class="hint" id="sendMsg">选好设备与目录后点发送；重复发送同一目录时，接收端会按内容跳过已经有的文件。</p>
  <p class="hint" id="sendProgress" style="display:none"></p>
  <button id="sendRetry" style="display:none;margin-top:8px">重试</button>
</section>

<section>
  <h2>历史（2 天）</h2>
  <table><thead><tr><th>时间</th><th>来源</th><th>落点</th><th>文件数</th><th>字节</th><th>结果</th></tr></thead><tbody id="history"><tr><td colspan="6" class="empty">暂无</td></tr></tbody></table>
</section>

<section>
  <h2>信任设备</h2>
  <p class="hint" id="trustMode">加载中</p>
  <table><thead><tr><th>别名</th><th>指纹</th><th>来源</th><th>首次批准</th><th>操作</th></tr></thead><tbody id="trust"><tr><td colspan="5" class="empty">加载中</td></tr></tbody></table>
  <h3>待确认设备</h3>
  <p class="hint">closed 模式下被拒绝、尚未批准的来源设备会出现在这里；批准后从列表消失。</p>
  <table><thead><tr><th>来源设备</th><th>指纹</th><th>最近尝试</th><th>原因</th><th>操作</th></tr></thead><tbody id="pending"><tr><td colspan="5" class="empty">加载中</td></tr></tbody></table>
  <div class="row mt">
    <div class="item"><span class="label">手动批准指纹</span><input type="text" id="newFp" placeholder="64 位十六进制指纹"></div>
    <div class="item"><span class="label">别名</span><input type="text" id="newAlias" placeholder="可选，如 客厅笔记本"></div>
    <button id="approve">批准</button>
  </div>
  <p class="hint" id="trustMsg">信任表只追加事件，撤销不会删除历史行；撤销后下一场会话即按新状态判定。</p>
</section>

<section>
  <h2>配对（扫码）</h2>
  <p class="hint">让对方用手机「扫二维码」直接配对，扫完就能发文件，不必再在信任表里手动批准。</p>
  <div class="row">
    <div class="item" style="flex:1"><span class="label">本机地址（自动判断，多网卡时改成对方能连到的那个）</span><input type="text" id="pairAddr" style="width:100%" placeholder="例如 192.168.1.23:53318"></div>
    <button id="pairGen">生成二维码</button>
    <button id="pairRevoke">立即作废</button>
  </div>
  <div id="pairBox" class="mt" style="display:none">
    <div id="pairQr" style="width:224px;height:224px;background:#fff;border:1px solid var(--line);border-radius:8px;padding:6px"></div>
    <p class="hint" id="pairInfo">—</p>
  </div>
  <p class="hint" id="pairMsg">尚未生成二维码。有效期 10 分钟，只能配一台设备；关闭本页不影响它继续可用。</p>
</section>

<section>
  <h2>设置</h2>
  <div class="row">
    <div class="item"><span class="label">设备别名</span><input type="text" id="setAlias"></div>
    <div class="item"><span class="label">自动接收（已配对设备）</span><input type="checkbox" id="setAuto"></div>
    <button id="save">保存</button>
  </div>
  <p class="hint" id="saveMsg">修改后立即生效，写入配置文件。</p>
</section>

<section>
  <h2>日志（2 天）</h2>
  <pre id="logs">加载中</pre>
</section>
</div>
<script>
function esc(s){return String(s).replace(/[&<>"']/g,function(c){return {'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]})}
function ts(v){var d=new Date(v*1000);return d.toLocaleString('zh-CN',{hour12:false})}
function fill(id,v){document.getElementById(id).textContent=v}
function rows(target,list,cols,empty){
  var el=document.getElementById(target);
  if(!list.length){el.innerHTML='<tr><td colspan="'+cols+'" class="empty">暂无</td></tr>';return}
  el.innerHTML=list.map(function(r){return '<tr>'+r.map(function(c){return '<td>'+esc(c)+'</td>'}).join('')+'</tr>'}).join('')
}
function status(){
  // 接口字段一律 snake_case（与 /api/history、/api/trust 保持一致），
  // 这里写成 camelCase 会拿到 undefined：时长显示 NaN、勾选框永远打不上勾
  fetch('/api/status').then(function(r){return r.json()}).then(function(s){
    fill('ver','v'+s.version+' · 在线');fill('alias',s.alias);fill('listen',s.listen);
    fill('uptime',Math.floor(s.uptime_secs/60)+' 分钟');fill('fp',s.fingerprint.slice(0,16)+'…');
    fill('current',s.current?'正在接收':'监听中，等待连接');
    rows('peers',s.peers.map(function(p){return [p.name,p.addr,ts(p.last_seen)]}),3);
    document.getElementById('setAlias').value=s.alias;
    document.getElementById('setAuto').checked=s.auto_accept;
  }).catch(function(){fill('ver','接口不可用')})
}
function history(){
  fetch('/api/history').then(function(r){return r.json()}).then(function(h){
    // 旧历史行没有来源/落点，按空值显示为「—」，不能因为缺字段就整行不显示
    rows('history',h.slice().reverse().map(function(e){
      var src=e.peer_name||'—';
      if(e.peer_addr)src+=' ('+e.peer_addr+')';
      if(e.peer_platform)src+=' · '+e.peer_platform;
      return [ts(e.ts),src,(e.roots&&e.roots.length?e.roots.join(', '):'—'),e.files,e.bytes,e.verified?'完成':'未完成'];
    }),6)
  })
}
function logs(){
  fetch('/api/logs').then(function(r){return r.json()}).then(function(l){
    fill('logs',l.length?l.map(function(x){return ts(x[0])+' ['+x[1]+'] '+x[2]}).join('\n'):'暂无')
  })
}
function trust(){
  fetch('/api/trust').then(function(r){return r.json()}).then(function(t){
    fill('trustMode','当前模式：'+t.mode+(t.mode==='open'?'（排障用，未信任设备也可发送，切换已记日志）':'（只放行信任表内的设备）'));
    var el=document.getElementById('trust');
    if(!t.peers.length){
      el.innerHTML='<tr><td colspan="5" class="empty">信任表为空，可在下方批准，或用 tc trust approve</td></tr>'
    }else{
      el.innerHTML=t.peers.map(function(p){
        var name=p.alias||p.name||'未命名';
        return '<tr><td>'+esc(name)+'</td><td title="'+esc(p.fp)+'">'+esc(p.fp.slice(0,16))+'…</td><td>'+esc(p.added_by||'—')+'</td><td>'+ts(p.first_seen)+'</td>'
          +'<td><button data-act="rename" data-fp="'+esc(p.fp)+'">改名</button> <button data-act="revoke" data-fp="'+esc(p.fp)+'">撤销</button></td></tr>'
      }).join('')
    }
    var pe=document.getElementById('pending');
    if(!t.pending||!t.pending.length){
      pe.innerHTML='<tr><td colspan="5" class="empty">暂无被拒尝试</td></tr>'
    }else{
      pe.innerHTML=t.pending.map(function(x){
        var name=x.name||'未命名';
        if(x.platform)name+=' · '+x.platform;
        return '<tr><td>'+esc(name)+'</td><td title="'+esc(x.fp)+'">'+esc(x.fp.slice(0,16))+'…</td><td>'+ts(x.ts)+'</td><td>'+esc(x.note||'—')+'</td>'
          +'<td><button data-act="approve" data-fp="'+esc(x.fp)+'">批准</button></td></tr>'
      }).join('')
    }
  }).catch(function(){fill('trustMode','信任接口不可用')})
}
function postTrust(body,msg){
  fetch('/api/trust',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)})
    .then(function(r){return r.json()}).then(function(j){
      fill(msg||'trustMsg',j.ok?(j.message||'已生效'):(j.error||'未生效'))
      trust()
    }).catch(function(){fill(msg||'trustMsg','请求失败')})
}
document.getElementById('trust').addEventListener('click',function(e){
  var b=e.target.closest('button[data-act]');if(!b)return
  var fp=b.getAttribute('data-fp'),act=b.getAttribute('data-act')
  if(act==='rename'){
    var a=prompt('新的别名（留空取消）');if(!a)return
    postTrust({action:'rename',fp:fp,alias:a})
  }else{
    if(!confirm('撤销后这台设备下次发送会被拒绝，确定？'))return
    postTrust({action:'revoke',fp:fp})
  }
})
document.getElementById('pending').addEventListener('click',function(e){
  var b=e.target.closest('button[data-act="approve"]');if(!b)return
  postTrust({action:'approve',fp:b.getAttribute('data-fp'),alias:''})
})
document.getElementById('approve').addEventListener('click',function(){
  var fp=document.getElementById('newFp').value.trim()
  if(!fp){fill('trustMsg','请先填指纹');return}
  postTrust({action:'approve',fp:fp,alias:document.getElementById('newAlias').value.trim()})
})
document.getElementById('save').addEventListener('click',function(){
  fetch('/api/settings',{method:'POST',headers:{'Content-Type':'application/json'},
    body:JSON.stringify({alias:document.getElementById('setAlias').value,autoAccept:document.getElementById('setAuto').checked})})
    .then(function(r){fill('saveMsg',r.ok?'已保存':'保存失败')})
})
// ---- 发送区（v0.10）：设备来自 /api/status，目录浏览走 /api/fs，进度轮询 /api/send/status ----
var sendPeers=[],browseCur='',browseParent='',lastSend=null
function loadTargets(){
  fetch('/api/status').then(function(r){return r.json()}).then(function(s){
    var keep=document.getElementById('sendTarget').value
    sendPeers=s.peers||[]
    var el=document.getElementById('sendTarget')
    if(!sendPeers.length){el.innerHTML='<option value="">（未发现设备，点「刷新设备」）</option>';return}
    el.innerHTML=sendPeers.map(function(p,i){return '<option value="'+i+'">'+esc(p.name)+' · '+esc(p.addr)+'</option>'}).join('')
    if(keep!==''&&el.querySelector('option[value="'+keep+'"]'))el.value=keep
  }).catch(function(){})
}
function browse(p){
  var box=document.getElementById('browse')
  // v0.13.6：目录树改成弹窗，打开时同时铺一层遮罩，点遮罩即关闭
  document.getElementById('browseMask').style.display='block'
  box.style.display='block'
  fill('browsePath','正在读取…')
  fetch('/api/fs?path='+encodeURIComponent(p||'')).then(function(r){return r.json()}).then(function(j){
    if(j.error){fill('browsePath','读取失败');fill('sendMsg','目录读取失败：'+j.error+'（可直接在输入框手输路径）');return}
    browseCur=j.path||''
    // 上级路径直接用后端算好的 parent：前端自己截字符串会在「D:\」这类根路径上截错
    browseParent=j.parent||''
    fill('browsePath',browseCur||'（磁盘 / 根目录）')
    document.getElementById('browseUp').style.display=browseParent?'inline-block':'none'
    var el=document.getElementById('browseList')
    var ds=j.dirs||[]
    el.innerHTML=ds.length?ds.map(function(d){
      // 后端给了显示名就用它（盘符会带卷标，如「D: 仓库」），否则退回路径最后一段
      var name=(j.labels&&j.labels[d])?j.labels[d]:(d.replace(/[\\/]+$/,'').split(/[\\/]/).pop()||d)
      return '<div style="margin:4px 0"><button data-dir="'+esc(d)+'" style="width:100%;text-align:left;background:var(--accent-soft);color:var(--ink)">'+esc(name)+'</button></div>'
    }).join(''):'<p class="empty">没有子目录</p>'
  }).catch(function(){fill('browsePath','读取失败');fill('sendMsg','目录读取失败，可直接在输入框手输路径')})
}
function closeBrowse(){
  document.getElementById('browse').style.display='none'
  document.getElementById('browseMask').style.display='none'
}
function startSend(addr,fp,path,force){
  lastSend={addr:addr,fp:fp,path:path,force:!!force}
  var pair=document.getElementById('sendPair').value.trim()
  var payload={to:addr,fp:fp,path:path}
  if(pair)payload.pairCode=pair
  if(force)payload.force=true
  fetch('/api/send',{method:'POST',headers:{'Content-Type':'application/json'},
    body:JSON.stringify(payload)})
    .then(function(r){return r.json()}).then(function(j){
      fill('sendMsg',j.ok?('已开始发送到 '+addr+'：'+path):('无法开始：'+(j.error||'未知原因')))
    }).catch(function(){fill('sendMsg','请求失败')})
}
function sendStatus(){
  fetch('/api/send/status').then(function(r){return r.json()}).then(function(j){
    var box=document.getElementById('sendProgress'),retry=document.getElementById('sendRetry')
    if(!j){box.style.display='none';retry.style.display='none';return}
    box.style.display='block'
    var mb=function(n){return (n/1048576).toFixed(1)+' MB'}
    var line
    if(j.phase==='scanning'){line='扫描中：'+j.files+' 个文件'+(j.total_bytes?' / '+mb(j.total_bytes):'')+'（正在算校验值）'}
    else if(j.phase==='sending'){line='传输中 '+j.percent+'% · '+mb(j.bytes_sent)+' / '+mb(j.total_bytes)+(j.speed_bps?' · '+(j.speed_bps/1048576).toFixed(1)+' MB/s':'')}
    else if(j.phase==='done'){line='已完成：'+j.files+' 个文件 / '+mb(j.total_bytes)+(j.files_skipped?'（接收端增量跳过 '+j.files_skipped+' 个）':'')}
    else{line='失败：'+(j.reason||'未知原因')}
    fill('sendProgress',line)
    retry.style.display=(j.phase==='failed'&&lastSend)?'inline-block':'none'
  }).catch(function(){})
}
document.getElementById('sendReload').addEventListener('click',function(){
  var el=document.getElementById('sendTarget')
  // 先给即时反馈：发现要等一个广播周期，没有提示会像按钮坏了（v0.13.1 修）
  el.innerHTML='<option value="">正在发现设备（最多 6 秒）…</option>'
  fill('sendMsg','正在监听局域网内的设备公告…')
  fetch('/api/discover').then(function(r){return r.json()}).then(function(list){
    if(!list||!list.length){
      fill('sendMsg','未发现设备：确认对端已开启接收、与本品在同一局域网，且防火墙放行 UDP 53318')
    }else{
      fill('sendMsg','发现 '+list.length+' 台设备')
    }
    loadTargets()
  }).catch(function(){fill('sendMsg','发现请求失败');loadTargets()})
})
document.getElementById('sendBrowse').addEventListener('click',function(){
  browse(document.getElementById('sendPath').value.trim())
})
document.getElementById('browseClose').addEventListener('click',closeBrowse)
document.getElementById('browseMask').addEventListener('click',closeBrowse)
document.getElementById('browseUp').addEventListener('click',function(){browse(browseParent)})
document.getElementById('browsePick').addEventListener('click',function(){
  document.getElementById('sendPath').value=browseCur
  closeBrowse()
})
document.getElementById('browseList').addEventListener('click',function(e){
  var b=e.target.closest('button[data-dir]');if(!b)return
  browse(b.getAttribute('data-dir'))
})
document.getElementById('sendGo').addEventListener('click',function(){
  var p=sendPeers[document.getElementById('sendTarget').value]
  var path=document.getElementById('sendPath').value.trim()
  if(!p){fill('sendMsg','先选一台设备（可点「刷新设备」）');return}
  if(!path){fill('sendMsg','先选一个目录');return}
  startSend(p.addr,p.fingerprint,path,document.getElementById('sendForce').checked)
})
document.getElementById('sendRetry').addEventListener('click',function(){
  if(lastSend)startSend(lastSend.addr,lastSend.fp,lastSend.path,lastSend.force)
})
// ---- 配对二维码（v0.20）：生成、倒计时、作废 ----
var pairTimer=null,pairExpires=0
function pairTick(){
  var left=Math.max(0,Math.floor(pairExpires-Math.floor(Date.now()/1000)))
  var m=Math.floor(left/60),s=left%60
  fill('pairInfo',left>0?('剩余 '+m+':'+('0'+s).slice(-2)+' · 一次性，扫过一次就失效'):'已过期，请重新生成')
}
function pairRender(j){
  var box=document.getElementById('pairBox')
  if(!j||!j.active){
    box.style.display='none'
    fill('pairMsg',(j&&j.reason)?j.reason:'当前没有有效二维码')
    return
  }
  document.getElementById('pairAddr').value=j.addr
  document.getElementById('pairQr').innerHTML=j.svg
  pairExpires=j.expires
  box.style.display='block'
  pairTick()
  if(pairTimer)clearInterval(pairTimer)
  pairTimer=setInterval(pairTick,1000)
}
function pairLoad(){
  fetch('/api/pair').then(function(r){return r.json()}).then(pairRender)
    .catch(function(){fill('pairMsg','配对接口不可用')})
}
document.getElementById('pairGen').addEventListener('click',function(){
  var addr=document.getElementById('pairAddr').value.trim()
  fetch('/api/pair',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({addr:addr})})
    .then(function(r){return r.json()}).then(function(j){
      if(j.ok){fill('pairMsg','二维码已生成，旧码已失效；给对方扫即可');pairRender(j)}
      else{fill('pairMsg','生成失败：'+(j.reason||'未知原因'))}
    }).catch(function(){fill('pairMsg','请求失败')})
})
document.getElementById('pairRevoke').addEventListener('click',function(){
  fetch('/api/pair/revoke',{method:'POST',headers:{'Content-Type':'application/json'},body:'{}'})
    .then(function(r){return r.json()}).then(function(j){
      document.getElementById('pairBox').style.display='none'
      if(pairTimer){clearInterval(pairTimer);pairTimer=null}
      fill('pairMsg',j.ok?'二维码已作废，任何人都不能再凭它配对':'作废失败')
    }).catch(function(){fill('pairMsg','请求失败')})
})
status();history();logs();trust();loadTargets();sendStatus();pairLoad()
setInterval(status,3000);setInterval(function(){history();logs();trust()},10000);setInterval(sendStatus,1000)
</script>
</body>
</html>
"##;

#[derive(Serialize)]
struct Status<'a> {
    version: &'a str,
    alias: String,
    listen: String,
    uptime_secs: i64,
    fingerprint: String,
    auto_accept: bool,
    current: Option<serde_json::Value>,
    peers: Vec<PeerInfo>,
}

// ── 管理页 API 响应契约（v0.21.0）
//
// 原先这些响应是 serde_json::json! 动态构造的，字段名只存在于字面量里：
// 改错字段名编译不报错、前端也看不出来，只能靠人肉对着页面数。
// 这里改为具名 struct，字段名写在 struct 上、由编译器保证序列化结构。
//
// 约束：字段名与原 json! 完全一致（含 camelCase，用 serde rename 保留），
// 不改变对外 JSON 形态；改字段名会同步改这里，前端契约一眼可查。

/// 目录浏览响应（GET /api/dirs）。
#[derive(Serialize)]
struct DirListResp {
    path: String,
    parent: Option<String>,
    dirs: Vec<String>,
    labels: HashMap<String, String>,
}

/// 通用错误响应（{"error": "..."}）。
#[derive(Serialize)]
struct ErrorResp {
    error: String,
}

/// 通用成功响应（{"ok": true}）。
#[derive(Serialize)]
struct OkResp {
    ok: bool,
}

/// 失败带错误（{"ok": false, "error": "..."}）。
#[derive(Serialize)]
struct OkErrorResp {
    ok: bool,
    error: String,
}

/// 配对不可用：自动判断局域网地址失败。
#[derive(Serialize)]
struct PairUnavailableResp {
    ok: bool,
    active: bool,
    reason: String,
}

/// 配对失败带原因（{"ok": false, "reason": "..."}）。
#[derive(Serialize)]
struct PairFailResp {
    ok: bool,
    reason: String,
}

/// 配对二维码已生成。
#[derive(Serialize)]
struct PairActiveResp {
    ok: bool,
    active: bool,
    addr: String,
    expires: i64,
    ttl: i64,
    remaining: i64,
    svg: String,
}

/// 配对未激活（{"ok": true, "active": false}）。
#[derive(Serialize)]
struct PairInactiveResp {
    ok: bool,
    active: bool,
}

/// 作废配对二维码的结果。
#[derive(Serialize)]
struct RevokeResp {
    ok: bool,
    revoked: bool,
}

/// 信任表状态（模式 + 已信任 + 待确认）。
#[derive(Serialize)]
struct TrustResp {
    mode: String,
    peers: Vec<TrustedPeer>,
    pending: Vec<TrustEvent>,
}

/// 带提示消息的成功响应（{"ok": true, "message": "..."}）。
#[derive(Serialize)]
struct MessageResp {
    ok: bool,
    message: String,
}

/// 管理页设置（GET /api/settings）。配对码属敏感配置，不返回。
#[derive(Serialize)]
struct SettingsResp {
    alias: String,
    #[serde(rename = "autoAccept")]
    auto_accept: bool,
    #[serde(rename = "trustMode")]
    trust_mode: String,
    destination: PathBuf,
    allow: Vec<PathBuf>,
    #[serde(rename = "logDays")]
    log_days: i64,
}

/// 写设置单独限流：管理页只监听局域网，避免被同网段反复刷配置。
struct RateLimit {
    hits: Mutex<Vec<i64>>,
}

impl RateLimit {
    fn allow(&self) -> bool {
        let now = now_secs();
        let mut h = self.hits.lock().expect("ratelimit poisoned");
        h.retain(|t| now - *t < 60);
        if h.len() >= 10 {
            return false;
        }
        h.push(now);
        true
    }
}

/// 管理页令牌：首次启动生成 32 字节系统随机数的十六进制串，存 identity 目录下的独立文件。
/// 不写进 config.toml —— 管理页改设置时会把配置整份重写（注释会丢），令牌也不该混在可读配置里。
pub fn load_or_create_token(identity_dir: &Path) -> io::Result<String> {
    let path = identity_dir.join("manage.token");
    if let Ok(t) = fs::read_to_string(&path) {
        let t = t.trim().to_string();
        // 太短的值视为损坏：宁可重新生成，也不要拿一个弱令牌当门禁
        if t.len() >= 32 {
            return Ok(t);
        }
    }
    let mut buf = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut buf)
        .map_err(|_| io::Error::other("生成管理页令牌失败"))?;
    let token: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    fs::create_dir_all(identity_dir)?;
    fs::write(&path, &token)?;
    restrict_permissions(&path)?;
    Ok(token)
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// 令牌的三种带法：`X-TC-Token` 头（脚本）、`Authorization: Bearer`（通用客户端）、`tc_token` Cookie（页面）。
/// `?t=` 是首次打开页面用的：校验通过后响应写 Cookie，之后前端 fetch 自动带上，不必每次拼查询串。
fn authorized(token: &str, headers: &HashMap<String, String>, query: &str) -> bool {
    if token.is_empty() {
        return true; // 未配置令牌时不启用鉴权，仅为兼容旧配置与单元测试
    }
    if let Some(v) = headers.get("x-tc-token") {
        if v.trim() == token {
            return true;
        }
    }
    if let Some(v) = headers.get("authorization") {
        if let Some(rest) = v.strip_prefix("Bearer ") {
            if rest.trim() == token {
                return true;
            }
        }
    }
    if let Some(v) = headers.get("cookie") {
        for part in v.split(';') {
            if let Some((k, val)) = part.split_once('=') {
                if k.trim() == "tc_token" && val.trim() == token {
                    return true;
                }
            }
        }
    }
    query_token(query) == Some(token)
}

fn query_token(query: &str) -> Option<&str> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        if k == "t" {
            Some(v)
        } else {
            None
        }
    })
}

/// 取普通查询参数（已做 URL 解码）。只处理本页需要的键，不做通用 querystring 解析。
fn query_value(query: &str, key: &str) -> String {
    query
        .split('&')
        .find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            if k == key {
                Some(url_decode(v))
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// 浏览器端的 encodeURIComponent 会把空格和中文都转成 %XX，这里还原。
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 目录浏览：给管理页的「浏览…」用。空路径列根：
/// - 配了 browse_roots 就只列这些目录（NAS 上不该让管理页翻到系统目录）；
/// - 没配时 Windows 列盘符（带卷标名），其它平台列 `/`。
/// 只列目录不列文件：发送单个文件时直接手输路径，避免列表被成千上万个文件刷屏。
fn list_dirs(path: &str, roots: &[PathBuf]) -> io::Result<DirListResp> {
    let mut dirs: Vec<String> = Vec::new();
    // 路径 → 显示名。前端优先用它，盘符也能显示成「D: 仓库」
    let mut labels: HashMap<String, String> = HashMap::new();
    let (cur, parent) = if path.trim().is_empty() {
        if !roots.is_empty() {
            for r in roots {
                if r.is_dir() {
                    let p = r.display().to_string();
                    if let Some(n) = r.file_name() {
                        labels.insert(p.clone(), n.to_string_lossy().to_string());
                    }
                    dirs.push(p);
                }
            }
        } else {
            #[cfg(windows)]
            {
                for letter in b'A'..=b'Z' {
                    let p = format!("{}:\\", letter as char);
                    if Path::new(&p).is_dir() {
                        // v0.13.3：盘符后带卷标名，用户不必逐个点开才知道是哪块盘
                        if let Some(label) = volume_label(&p) {
                            if !label.trim().is_empty() {
                                labels.insert(p.clone(), format!("{} {}", &p[..2], label.trim()));
                            }
                        }
                        dirs.push(p);
                    }
                }
            }
            #[cfg(not(windows))]
            {
                dirs.push("/".into());
            }
        }
        (String::new(), None)
    } else {
        let base = PathBuf::from(path);
        // 配了浏览根就限制在这一层内，避免管理页被当成任意文件系统浏览器
        if !roots.is_empty() && !within_roots(&base, roots) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "只能浏览配置里 browse_roots 列出的目录",
            ));
        }
        let parent = base.parent().map(|p| p.display().to_string());
        for entry in fs::read_dir(&base)? {
            let entry = entry?;
            // 跟随符号链接会跳出用户当前所在的树；只认真正的目录项
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            // v0.13.6：隐藏目录不列。管理页是给用户选发送目录的，不是文件管理器
            if is_hidden_dir(&entry.path()) {
                continue;
            }
            dirs.push(entry.path().display().to_string());
        }
        (base.display().to_string(), parent)
    };
    dirs.sort_by_key(|a| a.to_lowercase());
    // 上限只是防止一次返回几万条把页面卡死；正常目录远达不到
    dirs.truncate(500);
    Ok(DirListResp { path: cur, parent, dirs, labels })
}

/// 判断 path 是否落在任意一个浏览根内。规范化后再比，避免 `..` 跳出白名单。
fn within_roots(path: &Path, roots: &[PathBuf]) -> bool {
    let p = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    roots.iter().any(|r| {
        let r = r.canonicalize().unwrap_or_else(|_| r.clone());
        p == r || p.starts_with(&r)
    })
}

/// v0.13.6：隐藏目录不进目录树。
/// Unix 看名字前缀；Windows 再看隐藏属性——`System Volume Information` 这类没有点前缀，只能靠属性识别。
fn is_hidden_dir(path: &Path) -> bool {
    if let Some(name) = path.file_name() {
        if name.to_string_lossy().starts_with('.') {
            return true;
        }
    }
    #[cfg(windows)]
    {
        if let Some(attrs) = file_attributes(path) {
            return attrs & 0x2 != 0; // FILE_ATTRIBUTE_HIDDEN
        }
    }
    false
}

#[cfg(windows)]
fn file_attributes(path: &Path) -> Option<u32> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileAttributesW(name: *const u16) -> u32;
    }
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let a = unsafe { GetFileAttributesW(wide.as_ptr()) };
    // INVALID_FILE_ATTRIBUTES = u32::MAX
    if a == u32::MAX {
        None
    } else {
        Some(a)
    }
}

/// 取 Windows 盘符的卷标名；取不到（未格式化、无权限）就返回 None，不影响目录列表。
#[cfg(windows)]
fn volume_label(root: &str) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetVolumeInformationW(
            root: *const u16,
            name: *mut u16,
            name_len: u32,
            serial: *mut u32,
            max_component: *mut u32,
            flags: *mut u32,
            fs_name: *mut u16,
            fs_name_len: u32,
        ) -> i32;
    }
    let wide: Vec<u16> = std::ffi::OsStr::new(root)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut buf = [0u16; 128];
    let ok = unsafe {
        GetVolumeInformationW(
            wide.as_ptr(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    if ok == 0 {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    if end == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..end]))
}

pub fn serve(
    state: Arc<DaemonState>,
    addr: &str,
    fingerprint: String,
    token: Arc<String>,
) -> io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    let limit = Arc::new(RateLimit {
        hits: Mutex::new(Vec::new()),
    });
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let st = state.clone();
                let fp = fingerprint.clone();
                let lim = limit.clone();
                let tk = token.clone();
                thread::spawn(move || {
                    if let Err(e) = handle(stream, &st, &fp, &lim, &tk) {
                        st.log("warn", &format!("管理页连接异常: {e}"));
                    }
                });
            }
            Err(e) => state.log("warn", &format!("管理页 accept 失败: {e}")),
        }
    }
    Ok(())
}

fn handle(
    mut stream: TcpStream,
    state: &Arc<DaemonState>,
    fingerprint: &str,
    limit: &RateLimit,
    token: &str,
) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut first = String::new();
    if reader.read_line(&mut first)? == 0 {
        return Ok(());
    }
    let mut parts = first.trim_end().split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };

    let mut headers: HashMap<String, String> = HashMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len.min(64 * 1024)];
    if !body.is_empty() {
        reader.read_exact(&mut body)?;
    }

    // 健康检查保持匿名：它只回一个字面量 ok，运维探针和脚本都靠它判活
    let ok = path == "/api/health" || authorized(token, &headers, &query);
    let (code, ctype, payload) = if ok {
        route(state, &method, &path, &query, &body, fingerprint, limit)
    } else {
        (
            401,
            "text/plain",
            "401 未授权：请用 http://<本机地址>:53319/?t=<令牌> 打开管理页，令牌在 identity/manage.token\n"
                .as_bytes()
                .to_vec(),
        )
    };
    // 只在本次确实用了 ?t= 时下发 Cookie，避免把令牌写进每个响应头
    let set_cookie = if ok && query_token(&query) == Some(token) && !token.is_empty() {
        Some(format!("tc_token={token}; Path=/; HttpOnly; SameSite=Strict"))
    } else {
        None
    };
    // 排障线索只记路径不记 query：?t= 里带管理令牌，落盘等于泄露凭据（规范 #14）
    state.log("debug", &format!("管理页 {method} {path} -> {code}"));
    write_response(&mut stream, code, ctype, &payload, set_cookie.as_deref())
}

fn write_response(
    stream: &mut TcpStream,
    code: u16,
    ctype: &str,
    payload: &[u8],
    set_cookie: Option<&str>,
) -> io::Result<()> {
    let reason = match code {
        200 => "OK",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        _ => "Not Found",
    };
    let mut head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\n",
        payload.len()
    );
    if let Some(c) = set_cookie {
        head.push_str(&format!("Set-Cookie: {c}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(payload)?;
    stream.flush()
}

fn json<T: Serialize>(v: &T) -> (u16, &'static str, Vec<u8>) {
    (
        200,
        "application/json",
        serde_json::to_vec(v).unwrap_or_else(|_| b"{}".to_vec()),
    )
}

fn route(
    state: &Arc<DaemonState>,
    method: &str,
    path: &str,
    query: &str,
    body: &[u8],
    fingerprint: &str,
    limit: &RateLimit,
) -> (u16, &'static str, Vec<u8>) {
    match (method, path) {
        ("GET", "/") | ("GET", "/index.html") => {
            (200, "text/html", PAGE.as_bytes().to_vec())
        }
        ("GET", "/api/health") => (200, "text/plain", b"ok".to_vec()),
        ("GET", "/api/status") => {
            let cfg = state.config.lock().expect("config poisoned");
            let s = Status {
                version: env!("CARGO_PKG_VERSION"),
                alias: cfg.alias.clone(),
                listen: cfg.listen.clone(),
                uptime_secs: now_secs() - state.started_at,
                fingerprint: fingerprint.into(),
                auto_accept: cfg.auto_accept,
                current: state
                    .current
                    .lock()
                    .expect("current poisoned")
                    .as_ref()
                    .and_then(|c| serde_json::to_value(c).ok()),
                peers: state.visible_peers(),
            };
            json(&s)
        }
        ("GET", "/api/history") => json(&state.recent_history()),
        // 管理页「刷新设备」用：后台发现线程一直在听，这里等一个广播周期再返回最新列表。
        // 不在这里自己 bind 端口——两个 socket 抢同一个 UDP 端口时包只会流向其中一个，
        // 反而可能让常驻监听漏掉公告（v0.13.1 起改为等待后台线程收齐）。
        ("GET", "/api/discover") => {
            // 对端广播间隔是 5 秒，等 5.5 秒保证覆盖一整轮
            thread::sleep(std::time::Duration::from_millis(5500));
            json(&state.visible_peers())
        }
        // 目录浏览：管理页「浏览…」用，只列目录
        ("GET", "/api/fs") => {
            let want = query_value(query, "path");
            // 浏览根是配置项，每次请求读一次：改配置重启后立即生效，不必缓存
            let roots = state
                .config
                .lock()
                .expect("config poisoned")
                .browse_roots
                .clone();
            match list_dirs(&want, &roots) {
                Ok(v) => json(&v),
                Err(e) => json(&ErrorResp { error: e.to_string() }),
            }
        }
        // 最近一次发送任务；没有任务时返回 null（不是空对象），页面据此隐藏进度区
        ("GET", "/api/send/status") => {
            let job = state.send_job.lock().expect("send_job poisoned").clone();
            json(&job)
        }
        ("POST", "/api/send") => {
            if !limit.allow() {
                return (429, "text/plain", b"too many requests".to_vec());
            }
            let v: serde_json::Value = match serde_json::from_slice(body) {
                Ok(v) => v,
                Err(_) => return (400, "text/plain", b"bad json".to_vec()),
            };
            let to = v
                .get("to")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let path = v
                .get("path")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let fp = v
                .get("fp")
                .and_then(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            // 配对码只在用户显式填写时才用；不落盘、不回显
            let pair = v
                .get("pairCode")
                .and_then(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            // 强制重传（v0.12）：忽略接收端已有文件，本次全部重传
            let force = v.get("force").and_then(|x| x.as_bool()).unwrap_or(false);
            let req = crate::sendjob::SendRequest {
                to: to.clone(),
                fingerprint: fp,
                path: PathBuf::from(&path),
                pair_code: pair,
                force,
            };
            match crate::sendjob::start(state.clone(), req) {
                Ok(()) => {
                    // 路径与目标都记进日志：事后要能回答「管理页把什么发给了谁」
                    state.log(
                        "info",
                        &format!("管理页发起发送 目标={to} 路径={path} 强制重传={force}"),
                    );
                    json(&OkResp { ok: true })
                }
                Err(e) => (
                    400,
                    "application/json",
                    serde_json::to_vec(&OkErrorResp { ok: false, error: e.to_string() })
                        .unwrap_or_else(|_| b"{\"ok\":false}".to_vec()),
                ),
            }
        }
        // v0.20 配对二维码：GET 查看当前有效码，POST 生成新码（旧码立即失效）。
        // 地址以页面传入的为准（多网卡/NAS 多网口时自动判断可能挑错网卡）。
        ("GET", "/api/pair") | ("POST", "/api/pair") => {
            if method == "POST" && !limit.allow() {
                return (429, "text/plain", b"too many requests".to_vec());
            }
            let (listen, alias) = {
                let cfg = state.config.lock().expect("config poisoned");
                (cfg.listen.clone(), cfg.alias.clone())
            };
            let port = listen.rsplit(':').next().unwrap_or("53318").to_string();
            let given = if method == "POST" {
                serde_json::from_slice::<serde_json::Value>(body)
                    .ok()
                    .and_then(|v| v.get("addr").and_then(|x| x.as_str()).map(|s| s.trim().to_string()))
                    .unwrap_or_default()
            } else {
                query_value(query, "addr").trim().to_string()
            };
            let addr = if !given.is_empty() {
                given
            } else {
                // 自动判断失败时宁可报错让人手填，也不能把 127.0.0.1 或猜测的地址写进二维码
                match crate::pair::local_lan_addr() {
                    Some(ip) => format!("{ip}:{port}"),
                    None => String::new(),
                }
            };
            if addr.is_empty() {
                return json(&PairUnavailableResp {
                    ok: false,
                    active: false,
                    reason: "无法自动判断本机局域网地址，请在上方手填（例如 192.168.1.23:53318）".to_string(),
                });
            }
            let token = if method == "POST" {
                match state.pair.issue() {
                    Ok(t) => Some(t),
                    Err(e) => {
                        state.log("error", &format!("生成配对令牌失败: {e}"));
                        return (
                            500,
                            "application/json",
                            serde_json::to_vec(&PairFailResp { ok: false, reason: e.to_string() })
                                .unwrap_or_else(|_| b"{}".to_vec()),
                        );
                    }
                }
            } else {
                state.pair.active()
            };
            match token {
                Some(t) => {
                    let payload = crate::pair::PairPayload {
                        addr: addr.clone(),
                        fp: fingerprint.to_string(),
                        code: t.token.clone(),
                        exp: t.expires,
                        name: alias,
                    };
                    match crate::pair::qr_svg(&payload.encode(), 224) {
                        Ok(svg) => {
                            if method == "POST" {
                                // 只记地址与有效期，令牌本身绝不入日志
                                state.log(
                                    "info",
                                    &format!("已生成配对二维码：{addr}，{} 秒内有效", t.ttl),
                                );
                            }
                            json(&PairActiveResp {
                                ok: true,
                                active: true,
                                addr,
                                expires: t.expires,
                                ttl: t.ttl,
                                remaining: (t.expires - now_secs()).max(0),
                                svg,
                            })
                        }
                        Err(e) => {
                            state.log("error", &format!("渲染配对二维码失败: {e}"));
                            (
                                500,
                                "application/json",
                                serde_json::to_vec(&PairFailResp { ok: false, reason: e.to_string() })
                                    .unwrap_or_else(|_| b"{}".to_vec()),
                            )
                        }
                    }
                }
                None => json(&PairInactiveResp { ok: true, active: false }),
            }
        }
        ("POST", "/api/pair/revoke") => {
            let revoked = state.pair.revoke();
            state.log("info", "管理页作废了配对二维码");
            json(&RevokeResp { ok: true, revoked })
        }
        ("GET", "/api/trust") => {
            let mode = state.config.lock().expect("config poisoned").trust_mode.clone();
            // 读文件失败时不能把整页拖成 500：返回空表并留日志，用户至少还能看到模式
            let peers = match state.trust.list() {
                Ok(p) => p,
                Err(e) => {
                    state.log("warn", &format!("读取信任表失败: {e}"));
                    Vec::new()
                }
            };
            let pending = state.trust.pending().unwrap_or_default();
            json(&TrustResp { mode, peers, pending })
        }
        ("POST", "/api/trust") => {
            if !limit.allow() {
                return (429, "text/plain", b"too many requests".to_vec());
            }
            let v: serde_json::Value = match serde_json::from_slice(body) {
                Ok(v) => v,
                Err(_) => return (400, "text/plain", b"bad json".to_vec()),
            };
            let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
            let fp = v.get("fp").and_then(|x| x.as_str()).unwrap_or("");
            let alias = v.get("alias").and_then(|x| x.as_str()).unwrap_or("");
            let short: String = fp.chars().take(12).collect();
            let result = match action {
                "approve" => state
                    .trust
                    .approve(fp, alias, "", "", "manual")
                    .map(|_| format!("已批准 {short}")),
                "revoke" => state.trust.revoke(fp, "manual", "管理页撤销").map(|ok| {
                    if ok {
                        format!("已撤销 {short}")
                    } else {
                        format!("{short} 不在信任表")
                    }
                }),
                "rename" => state.trust.rename(fp, alias).map(|ok| {
                    if ok {
                        format!("已改名 {short}")
                    } else {
                        format!("{short} 不在信任表")
                    }
                }),
                _ => return (400, "text/plain", b"unknown action".to_vec()),
            };
            match result {
                Ok(message) => {
                    state.log("info", &format!("管理页信任操作: {message}"));
                    json(&MessageResp { ok: true, message })
                }
                Err(e) => {
                    // 指纹格式错误、磁盘不可写都会走到这里，要把原因回给页面而不是只说失败
                    state.log("warn", &format!("管理页信任操作失败 action={action} fp={short}: {e}"));
                    (
                        400,
                        "application/json",
                        serde_json::to_vec(&OkErrorResp { ok: false, error: e.to_string() })
                            .unwrap_or_else(|_| b"{\"ok\":false}".to_vec()),
                    )
                }
            }
        }
        ("GET", "/api/logs") => {
            let lines: Vec<(i64, String, String)> = state.recent_logs();
            json(&lines)
        }
        ("GET", "/api/settings") => {
            // 配对码属于敏感配置，管理页一律不返回
            let cfg = state.config.lock().expect("config poisoned");
            json(&SettingsResp {
                alias: cfg.alias.clone(),
                auto_accept: cfg.auto_accept,
                trust_mode: cfg.trust_mode.clone(),
                destination: cfg.destination.clone(),
                allow: cfg.allow.clone(),
                log_days: cfg.log_days,
            })
        }
        ("POST", "/api/settings") => {
            if !limit.allow() {
                return (429, "text/plain", b"too many requests".to_vec());
            }
            let v: serde_json::Value = match serde_json::from_slice(body) {
                Ok(v) => v,
                Err(_) => return (400, "text/plain", b"bad json".to_vec()),
            };
            let mut mode_change: Option<(String, String)> = None;
            {
                let mut cfg = state.config.lock().expect("config poisoned");
                if let Some(a) = v.get("alias").and_then(|x| x.as_str()) {
                    cfg.alias = a.trim().chars().take(64).collect();
                }
                if let Some(a) = v.get("autoAccept").and_then(|x| x.as_bool()) {
                    cfg.auto_accept = a;
                }
                // 从 open 切回 closed 由这里负责留痕；非法值直接忽略，避免写进配置后启动报错
                if let Some(m) = v.get("trustMode").and_then(|x| x.as_str()) {
                    let m = m.trim().to_ascii_lowercase();
                    if (m == "closed" || m == "open") && m != cfg.trust_mode {
                        mode_change = Some((cfg.trust_mode.clone(), m.clone()));
                        cfg.trust_mode = m;
                    }
                }
                if let Err(e) = cfg.save(&state.config_path) {
                    state.log("error", &format!("保存配置失败: {e}"));
                    return (500, "text/plain", b"save failed".to_vec());
                }
            }
            if let Some((from, to)) = mode_change {
                // open 等于临时放开落盘入口，必须留 warn 级日志，事后能查出谁在什么时候切的
                state.log("warn", &format!("信任模式由 {from} 切到 {to}（管理页设置）"));
            }
            state.log("info", "管理页更新了设置");
            (200, "application/json", b"{\"ok\":true}".to_vec())
        }
        _ => (404, "text/plain", b"not found".to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::Config;

    fn fixture(name: &str) -> (Arc<DaemonState>, RateLimit) {
        let dir = std::env::temp_dir().join(format!("tc-web-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.alias = "验收设备".into();
        cfg.destination = dir.join("received");
        cfg.identity_dir = dir.join("identity");
        cfg.log_dir = dir.join("logs");
        cfg.queue_dir = dir.join("queue");
        cfg.allow = vec![dir.join("received")];
        cfg.pair_code = Some("secret123".into());
        (
            Arc::new(DaemonState::new(cfg, dir.join("config.toml")).unwrap()),
            RateLimit {
                hits: Mutex::new(Vec::new()),
            },
        )
    }

    /// 测试基本不关心查询串：统一走这个包装，避免每个用例都多传一个空参数
    fn call(
        st: &Arc<DaemonState>,
        method: &str,
        path: &str,
        body: &[u8],
        fp: &str,
        lim: &RateLimit,
    ) -> (u16, &'static str, Vec<u8>) {
        route(st, method, path, "", body, fp, lim)
    }

    #[test]
    fn management_endpoints_return_real_fields_and_hide_pair_code() {
        let (st, lim) = fixture("m1");
        let (code, _, body) = call(&st, "GET", "/", b"", "fp-abc", &lim);
        assert_eq!(code, 200);
        assert!(String::from_utf8_lossy(&body).contains("通传"));

        let (code, _, body) = call(&st, "GET", "/api/status", b"", "fp-abc", &lim);
        assert_eq!(code, 200);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(v["alias"], "验收设备");
        assert_eq!(v["fingerprint"], "fp-abc");

        // 接口冒烟不能只看「返回 200」，要确认敏感配置真的没漏出来
        let (_, _, body) = call(&st, "GET", "/api/settings", b"", "fp-abc", &lim);
        let text = String::from_utf8_lossy(&body);
        assert!(!text.contains("secret123"));
        assert!(!text.contains("pair_code"));

        let (code, _, _) = call(
            &st,
            "POST",
            "/api/settings",
            r#"{"alias":"新名字","autoAccept":false}"#.as_bytes(),
            "fp-abc",
            &lim,
        );
        assert_eq!(code, 200);
        let cfg = st.config.lock().unwrap();
        assert_eq!(cfg.alias, "新名字");
        assert!(!cfg.auto_accept);
    }

    #[test]
    fn settings_writes_are_rate_limited() {
        let (st, lim) = fixture("m2");
        let mut last = 0;
        for _ in 0..11 {
            let (code, _, _) = call(
                &st,
                "POST",
                "/api/settings",
                br#"{"alias":"x"}"#,
                "fp",
                &lim,
            );
            last = code;
        }
        assert_eq!(last, 429);
    }

    #[test]
    fn trust_api_reflects_writes_immediately() {
        let (st, lim) = fixture("trust");
        let (code, _, body) = call(&st, "GET", "/api/trust", b"", "fp", &lim);
        assert_eq!(code, 200);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["mode"], "closed");
        assert_eq!(v["peers"].as_array().unwrap().len(), 0);

        // 接口批准后下一次 GET 就要看到；撤销后同理，不依赖重启
        let fp = "d4".repeat(32);
        let approve = format!(r#"{{"action":"approve","fp":"{fp}","alias":"接口设备"}}"#);
        let (code, _, _) = call(&st, "POST", "/api/trust", approve.as_bytes(), "fp", &lim);
        assert_eq!(code, 200);
        let (_, _, body) = call(&st, "GET", "/api/trust", b"", "fp", &lim);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["peers"].as_array().unwrap().len(), 1);
        assert_eq!(v["peers"][0]["alias"], "接口设备");

        let revoke = format!(r#"{{"action":"revoke","fp":"{fp}"}}"#);
        let (code, _, _) = call(&st, "POST", "/api/trust", revoke.as_bytes(), "fp", &lim);
        assert_eq!(code, 200);
        let (_, _, body) = call(&st, "GET", "/api/trust", b"", "fp", &lim);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["peers"].as_array().unwrap().len(), 0);

        // 非法指纹要明确 400，不能静默成功让用户以为批准了
        let (code, _, _) = call(
            &st,
            "POST",
            "/api/trust",
            br#"{"action":"approve","fp":"short"}"#,
            "fp",
            &lim,
        );
        assert_eq!(code, 400);
    }

    /// 令牌三种携带方式都要认，且错误令牌必须拒绝；空令牌（旧配置）不能把页面锁死。
    #[test]
    fn token_auth_accepts_all_three_carriers_and_rejects_wrong() {
        let token = "a".repeat(64);
        assert!(!authorized(&token, &HashMap::new(), ""));

        let mut h = HashMap::new();
        h.insert("x-tc-token".into(), token.clone());
        assert!(authorized(&token, &h, ""));

        let mut h2 = HashMap::new();
        h2.insert("authorization".into(), format!("Bearer {token}"));
        assert!(authorized(&token, &h2, ""));

        let mut h3 = HashMap::new();
        h3.insert("cookie".into(), format!("a=1; tc_token={token}"));
        assert!(authorized(&token, &h3, ""));

        assert!(authorized(&token, &HashMap::new(), &format!("t={token}")));
        assert!(!authorized(&token, &HashMap::new(), "t=wrong"));
        assert!(authorized("", &HashMap::new(), ""));
    }

    /// 令牌必须落盘且重启后复用：每次启动换新令牌会让书签和脚本全部失效。
    #[test]
    fn token_file_is_stable_and_hex() {
        let dir = std::env::temp_dir().join(format!("tc-token-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = load_or_create_token(&dir).unwrap();
        let b = load_or_create_token(&dir).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// v0.10：目录浏览与发送接口的守门条件。
    /// 没有指纹、路径不存在都必须明确拒绝——不能默默起一个永远连不上的发送任务。
    #[test]
    fn fs_and_send_endpoints_guard_inputs() {
        let (st, lim) = fixture("sendapi");
        let dir = std::env::temp_dir();
        let query = format!("path={}", dir.display());
        let (code, _, body) = route(&st, "GET", "/api/fs", &query, b"", "fp", &lim);
        assert_eq!(code, 200);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["dirs"].is_array(), "目录列表必须是数组：{v}");

        let (code, _, body) = call(
            &st,
            "POST",
            "/api/send",
            br#"{"to":"127.0.0.1:53318","path":"."}"#,
            "fp",
            &lim,
        );
        assert_eq!(code, 400);
        assert!(String::from_utf8_lossy(&body).contains("指纹"));

        let missing = dir.join("tc-web-not-exist-zzz");
        let payload = format!(
            r#"{{"to":"127.0.0.1:53318","fp":"{}","path":{}}}"#,
            "ab".repeat(32),
            serde_json::to_string(&missing.display().to_string()).unwrap()
        );
        let (code, _, _) = call(&st, "POST", "/api/send", payload.as_bytes(), "fp", &lim);
        assert_eq!(code, 400);
        assert!(st.send_job.lock().unwrap().is_none(), "被拒绝的请求不该留下任务");
    }

    /// v0.20：配对二维码的生成、查看与作废。
    /// 关键点：POST 会换新码（旧码立刻失效），GET 不能换码，作废后不再返回二维码。
    #[test]
    fn pair_qr_endpoint_issues_rotates_and_revokes() {
        let (st, lim) = fixture("pair");
        let (code, _, body) = call(
            &st,
            "POST",
            "/api/pair",
            br#"{"addr":"192.168.1.9:53318"}"#,
            "fp-pair",
            &lim,
        );
        assert_eq!(code, 200);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["active"], true);
        assert_eq!(v["addr"], "192.168.1.9:53318");
        assert!(v["svg"].as_str().unwrap().contains("<svg"));
        let first_exp = v["expires"].as_i64().unwrap();
        let first_code = st.pair.active().unwrap().token;

        // GET 只是查看，必须沿用同一次性令牌
        let (_, _, body) = call(&st, "GET", "/api/pair", b"", "fp-pair", &lim);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["active"], true);
        assert_eq!(v["expires"].as_i64().unwrap(), first_exp);
        assert_eq!(st.pair.active().unwrap().token, first_code);

        // 再次 POST 是「重新生成」：旧码必须作废
        let (_, _, _) = call(
            &st,
            "POST",
            "/api/pair",
            br#"{"addr":"192.168.1.9:53318"}"#,
            "fp-pair",
            &lim,
        );
        let second = st.pair.active().unwrap().token;
        assert_ne!(first_code, second);

        let (_, _, body) = call(&st, "POST", "/api/pair/revoke", b"{}", "fp-pair", &lim);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["ok"], true);
        assert!(st.pair.active().is_none());
        let (_, _, body) = call(&st, "GET", "/api/pair", b"", "fp-pair", &lim);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["active"], false);
    }

    /// 配对的页面区块与脚本必须真的在页面上，不能只写了后端接口。
    #[test]
    fn page_contains_pair_section() {
        assert!(PAGE.contains("配对（扫码）"));
        assert!(PAGE.contains("/api/pair"));
        assert!(PAGE.contains("立即作废"));
    }

    #[test]
    fn page_follows_design_floor() {
        // 纯 Rust 自检代替 Node 版 sloplint：项目禁止引入 Node 检查脚本
        assert!(PAGE.contains(":root{"));
        assert!(PAGE.contains(":focus-visible"));
        assert!(PAGE.contains("prefers-reduced-motion"));
        assert!(PAGE.contains("overflow-x:clip"));
        assert!(PAGE.contains("信任设备"));
        assert!(!PAGE.contains("transition: all"));
        assert!(!PAGE.contains("100vw"));
        assert!(!PAGE.contains("<script src"));
        assert!(!PAGE.contains("https://"));
    }
}
