import { describe, expect, test } from 'bun:test';
import { ApiError } from '../runtime/api';
import { integrationHttp } from './http';

const answer = (status: number, body: string) => async () =>
  new Response(body, { status, headers: { 'Content-Type': 'application/json' } });

describe('the refusal of a project call', () => {
  test('a 400 keeps its status, message, error code and field', async () => {
    const http = integrationHttp(
      answer(
        400,
        JSON.stringify({
          error: 'invalid_git_settings',
          field: 'push_remote',
          message: 'The push remote must be a remote name, for example origin.',
        }),
      ),
      () => 'token',
      1000,
    );
    const failed = await http.put('/api/projects/p1/git', {}).catch((e: unknown) => e);
    expect(failed).toBeInstanceOf(ApiError);
    const error = failed as ApiError;
    expect(error.status).toBe(400);
    expect(error.code).toBe('invalid_git_settings');
    expect(error.field).toBe('push_remote');
    expect(error.message).toBe('The push remote must be a remote name, for example origin.');
  });

  test('an answer without error or field leaves both null', async () => {
    const http = integrationHttp(answer(500, 'not json'), () => null, 1000);
    const error = (await http.del('/api/projects/p1').catch((e: unknown) => e)) as ApiError;
    expect(error.code).toBeNull();
    expect(error.field).toBeNull();
    expect(error.message).toBe('Request failed (500)');
  });
});
