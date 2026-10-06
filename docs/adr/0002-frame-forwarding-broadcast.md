# ADR-0002：转发协议用广播模型，不用分角色的发布/订阅

- 状态：已采纳
- 日期：2026-10-06
- 关联：`SecRelay-Relay/docs/adr/0001-media-fallback-sfu.md`

## 背景

ADR-0001 决定兜底改为经信令通道由服务端转发密文帧。最初设计的协议是**分角色**的：

```
Publish  / PublishAnswer  / Subscribe{session_id} / Subscribed / Frame
一个会话只允许一个发布者
```

这个模型**实现不了**：`secrelay-session` 建会话时要走**双向握手**（`Hello` → `HelloAck`）。
单向通道完不成握手 —— 而这是现有公开 API，不该为了转发协议去改它。

## 决策

改为**广播模型**。会话就是一个组，**任意成员都可以发帧**，服务端把每帧广播给同会话的其它成员。

```
Client → Server:  Join { session_id }
                  Leave { session_id }
                  Frame { session_id, payload }

Server → Client:  Joined { session_id }
                  Left { session_id }
                  PeerJoined { session_id, from }
                  PeerLeft { session_id, from }
                  Frame { session_id, from, payload }
                  Error { code, message }
```

规则：

- 一个会话可有多个成员；成员上限要有明确值，超限返回 `Error`
- 广播**不发给发送者自己**
- 成员离开、断线、会话被 GC 时都要清理订阅关系，**不留悬挂引用**
- 帧大小有上限，超限返回 `Error` 而不是 panic
- 转发字节**计入**每 IP 每日配额（否则是新白嫖入口）
- `payload` 是**不透明字节**，服务端不解析、不记录内容

## 为什么广播比发布/订阅合适

- **双向握手成立**：两端都能发帧，才能完成 `Hello`/`HelloAck`
- **1:N 与 N:M 同时成立**：一台设备被多台观看是主要场景，而双向消息又要求反向通道
- **服务端更简单**：不需要维护"谁是发布者"的角色状态，只维护成员集合

## 代价

- 服务端对帧内容一无所知，因此**无法按角色做差异化转发或转码**。
  这是刻意的：零知识是硬约束，服务端本来就不该看内容。
- 广播模型下**无法区分"媒体流"和"控制帧"**，限额只能按字节总数算，不能按流算。

## 已实测的支撑结论

移动网络（实测中国移动 CGNAT，出口 `39.144.228.240`）**打洞在原理上不可能成功**：
两端都拿到了 srflx 候选，直连仍然失败。因此转发兜底不是优化项，是必需品。

同一轮实测里，家庭宽带 ↔ 有公网 IP 的机器是**直连成功**的（`是否经中继：false`），
说明直连路径本身没问题，失败的原因是 NAT 组合而非实现缺陷。
