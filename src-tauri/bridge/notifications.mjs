import { randomUUID } from 'node:crypto';

export const name = '@dsh-launcher/notifications';

/** 仅观察事件，不读取宿主服务或改变问题的回答结果。 */
export function apply(ctx, config) {
  const pending = new Map();
  const completed = new Map();
  const running = new Set();
  const parents = new Map();
  const write = (sessionId, id, kind) => {
    process.stdout.write(`${config.marker}${JSON.stringify({ sessionId, id, kind })}\n`);
  };
  const flushCompleted = () => {
    for (const [sessionId, id] of completed) {
      const active = [...running].some(child => {
        let current = child;
        while (current) {
          if (current === sessionId) {
            return true;
          }
          current = parents.get(current);
        }
        return false;
      });
      if (!active && ![...pending.values()].some(item => item.sessionId === sessionId)) {
        completed.delete(sessionId);
        write(sessionId, id, 'completed');
      }
    }
  };
  const observeAgent = ({ agent }) => {
    const session = agent.session;
    if (session.header.origin === 'subagent' && session.header.parentSession) {
      parents.set(session.id, session.header.parentSession);
    }
    if (agent.status === 'running') {
      running.add(session.id);
    } else {
      running.delete(session.id);
    }
    flushCompleted();
  };
  ctx.on('agent/created', observeAgent, { global: true });
  ctx.on('agent/status', observeAgent, { global: true });
  ctx.on('agent/disposed', ({ agent }) => {
    running.delete(agent.session.id);
    completed.delete(agent.session.id);
    flushCompleted();
  }, { global: true });
  ctx.on('user-questions/request', async (request, next) => {
    const sessionId = request.agent?.session.id;
    const questions = request.questions;
    if (!sessionId || !Array.isArray(questions)) {
      return next();
    }
    const kind = questions.some(question => question.intent?.kind === 'plan-review')
      ? 'plan'
      : questions.every(question => question.intent?.kind === 'approval') ? null : 'question';
    if (!kind) {
      return next();
    }
    const id = request.wait?.callId ?? randomUUID();
    pending.set(id, { sessionId, kind });
    write(sessionId, id, kind);
    let answered = false;
    try {
      const answer = await next();
      answered = true;
      return answer;
    } finally {
      // 限时问题在超时后仍可回答，由后续工具结果或用户回复决定是否关闭。
      if (answered || !request.wait?.timed) {
        pending.delete(id);
        write(sessionId, id, 'resolved');
      }
    }
  }, { global: true, prepend: true });
  ctx.on('session/event', (session, event) => {
    if (session.header.origin === 'subagent') {
      return;
    }
    let resolved;
    if (event.type === 'user/message' && event.data.source?.kind === 'user-question-reply') {
      resolved = event.data.source.callId;
    } else if (event.type === 'tool/result' && pending.has(event.data.message.toolCallId)) {
      const content = event.data.message.content.find(block => block.type === 'text');
      let continued = false;
      try {
        continued = content && JSON.parse(content.text).pending === true;
      } catch (error) {
        // 普通失败文本表示问题已结束，保留诊断以便核对宿主结果格式。
        ctx.logger?.debug('问题工具结果不含 JSON: %s', error.message);
      }
      if (!continued) {
        resolved = event.data.message.toolCallId;
      }
    }
    if (resolved && pending.delete(resolved)) {
      write(session.id, resolved, 'resolved');
    }
    if (event.type === 'turn/start') {
      completed.delete(session.id);
      running.add(session.id);
      write(session.id, `turn:${event.data.turn}`, 'running');
    } else if (event.type === 'turn/end') {
      // 一轮结束后仍可能处理队列或子代理，等代理及其后代空闲再提醒。
      if (event.data.reason.kind === 'completed') {
        completed.set(session.id, `turn:${event.data.turn}`);
      } else {
        completed.delete(session.id);
      }
    }
  }, { global: true });
  ctx.effect(() => () => {
    for (const [id, { sessionId }] of pending) {
      write(sessionId, id, 'resolved');
    }
    pending.clear();
    completed.clear();
    running.clear();
    parents.clear();
  }, 'launcher-notifications: 清除未处理请求');
}
