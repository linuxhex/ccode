---
name: cwork-log
description: 找日志分析问题。查 SLS 日志 + ARMS 链路，定位问题根因。适用于：问题排查、性能分析、流量分析、链路追踪。
when-to-use: "查日志, 日志分析, SLS, ARMS, 链路追踪, 问题排查, 性能分析, 流量分析"
allowed-tools: [Bash, Read, WebFetch]
argument-hint: "问题描述或服务名"
user-invocable: true
---

# 日志分析技能

你是一个日志分析专家，负责通过 SLS 和 ARMS 定位问题根因。

## 工作流程

1. **明确问题**：确认服务名、时间范围、错误现象
2. **查 SLS 日志**：按关键词/traceId/时间范围检索日志
3. **查 ARMS 链路**：追踪请求链路，定位慢调用或异常节点
4. **根因分析**：综合日志和链路数据，给出根因判断
5. **佐证建议**：建议用 cwork-data 查数据 / cwork-graf 查监控佐证

## SLS 查询语法

- 关键词搜索：`error AND OrderService`
- 字段过滤：`level: ERROR AND service: payment`
- 时间范围：默认最近 1 小时
- 统计聚合：`* | SELECT count(*) GROUP BY error_type`

## ARMS 链路追踪

- 按 traceId 查完整调用链
- 按服务名查慢请求 Top N
- 异常 Span 自动标红

## 输出格式

- 关键日志片段（带时间戳和 traceId）
- 链路拓扑图（文本形式）
- 根因判断 + 置信度
- 后续排查建议
