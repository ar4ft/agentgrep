"""Real subprocess protocol regression tests, run after cargo test --locked."""
import json
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

BINARY = Path(os.environ.get('AGX_TEST_BINARY', Path(__file__).resolve().parents[1] / 'target/debug/agx')).resolve()

class Worker:
    def __init__(self, root, limits=None, env=None, other_roots=None):
        self.process = subprocess.Popen([str(BINARY), 'serve', '--stdio', '--restricted'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
        self.messages = queue.Queue()
        self.notifications = []
        self.next_id = 1
        def read():
            for line in self.process.stdout:
                try: self.messages.put(json.loads(line))
                except Exception as error: self.messages.put(error)
        self.reader = threading.Thread(target=read, daemon=True)
        self.reader.start()
        roots = [{'id':'root','path':str(root)}] + (other_roots or [])
        params = {'protocol_version':1,'workspace_id':'nain-fixture','roots':roots}
        if limits: params['limits'] = limits
        self.info = self.call('initialize', params)['result']
    def send(self, method, params, request_id=None):
        if request_id is None:
            request_id = self.next_id
            self.next_id += 1
        self.process.stdin.write(json.dumps({'id':request_id,'method':method,'params':params})+'\n')
        self.process.stdin.flush()
        return request_id
    def wait(self, request_id):
        while True:
            message = self.messages.get(timeout=20)
            if isinstance(message, Exception): raise message
            assert message['protocol_version'] == 1, message
            if message.get('id') == request_id: return message
            if 'method' in message: self.notifications.append(message)
            else: raise AssertionError(f'unexpected response: {message}')
    def call(self, method, params): return self.wait(self.send(method, params))
    def index(self, method='index/refresh'): return self.call(method, {'root_id':'root'})['result']
    def search(self, query, mode='text', **params): return self.call('search', {'root_id':'root','query':query,'mode':mode,**params})
    def close(self):
        self.process.stdin.close()
        self.process.wait(timeout=20)
        self.reader.join(timeout=2)
        stderr = self.process.stderr.read()
        self.process.stdout.close(); self.process.stderr.close()
        assert self.process.returncode == 0, stderr
        assert stderr == '', stderr

class WorkerTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
    def write(self, path, content):
        file = self.root / path; file.parent.mkdir(parents=True, exist_ok=True); file.write_text(content)
    def worker(self, **kwargs):
        worker = Worker(self.root, **kwargs); self.addCleanup(worker.close); return worker
    def test_negotiation_and_cli_compatibility(self):
        self.write('App.swift', 'func findNeedle() {\n    print("needle")\n}\n')
        w = self.worker()
        self.assertEqual(w.info['search_modes'], ['text','symbol','ranked'])
        self.assertFalse(w.info['capabilities']['network'])
        self.assertEqual(w.search('needle')['error']['code'], 'index_not_ready')
        indexed = w.index(); self.assertEqual(indexed['files_read'], 1)
        self.assertEqual([n['params']['phase'] for n in w.notifications], ['started','complete'])
        for mode in ('text','symbol','ranked'):
            r = w.search('needle',mode)['result']
            self.assertEqual(r['schema_version'],2)
            self.assertEqual(r['workspace_id'],'nain-fixture')
            self.assertEqual(r['root_id'],'root')
            self.assertEqual(r['results'][0]['path'],'App.swift')
            self.assertEqual(r['results'][0]['content_hash'].__len__(),64)
            self.assertEqual(r['results'][0]['source_range']['start']['byte_column'],0)
            self.assertEqual(r['results'][0]['source_range']['end']['line'],r['results'][0]['end_line'])
            if mode == 'symbol': self.assertEqual(r['results'][0]['symbol'],'findNeedle')
            cli = subprocess.run([str(BINARY),'search','needle',str(self.root),'--mode',mode],capture_output=True,text=True,check=True)
            old = json.loads(cli.stdout)
            self.assertEqual(old['schema_version'],1)
            self.assertEqual(old['results'][0]['path'],r['results'][0]['path'])
            self.assertEqual(old['results'][0]['start_line'],r['results'][0]['start_line'])
        self.assertEqual(w.index()['files_read'],0)
        self.assertEqual(w.index()['files_reused'],1)
        (self.root/'App.swift').unlink()
        self.assertEqual(w.search('needle')['result']['matched_units'],1)
        w.call('workspace/files_changed',{'root_id':'root','paths':['App.swift']})
        self.assertEqual(w.search('needle')['result']['matched_units'],0)
    def test_overlays_versions_save_close_delete_and_stale_fences(self):
        self.write('app.py','def old():\n    return "disk_token"\n')
        w = self.worker(); w.index()
        v = w.call('document/update',{'root_id':'root','path':'app.py','version':5,'content':'def fresh():\n    return "overlay_token"\n'})['result']['index_version']
        hit = w.search('overlay_token','symbol',expected_index_version=v)['result']['results'][0]
        self.assertEqual(hit['symbol'],'fresh'); self.assertEqual(hit['document_version'],5)
        self.assertEqual(hit['source'],'overlay')
        self.assertEqual(w.search('disk_token')['result']['matched_units'],0)
        self.assertEqual(w.search('overlay_token',expected_index_version=v-1)['error']['code'],'stale_index')
        stale = w.call('document/update',{'root_id':'root','path':'app.py','version':4,'content':'wrong'})
        self.assertEqual(stale['error']['code'],'stale_document')
        self.write('app.py','def saved():\n    return "saved_token"\n')
        w.call('workspace/files_changed',{'root_id':'root','paths':['app.py']})
        self.assertEqual(w.search('overlay_token')['result']['matched_units'],1)
        self.assertEqual(w.call('document/close',{'root_id':'root','path':'app.py','version':4})['error']['code'],'stale_document')
        w.call('document/save',{'root_id':'root','path':'app.py','version':5})
        self.assertEqual(w.search('saved_token','symbol')['result']['results'][0]['symbol'],'saved')
        self.assertIsNone(w.search('saved_token')['result']['results'][0]['document_version'])
        self.assertEqual(w.call('document/update',{'root_id':'root','path':'app.py','version':5,'content':'wrong'})['error']['code'],'stale_document')
        (self.root/'app.py').unlink()
        w.call('workspace/files_changed',{'root_id':'root','paths':['app.py']})
        self.assertEqual(w.search('saved_token')['result']['matched_units'],0)
    def test_unsaved_new_file_ignore_rule_changes_and_language_filters(self):
        self.write('app.py','needle python')
        self.write('App.swift','func needle() {}')
        self.write('ignored/no.py','needle hidden')
        self.write('.gitignore','ignored/\n')
        w = self.worker(); w.index()
        w.call('document/update',{'root_id':'root','path':'new.py','version':1,'content':'needle unsaved'})
        r = w.search('needle',languages=['python'])['result']
        self.assertEqual({x['path'] for x in r['results']},{'app.py','new.py'})
        self.assertEqual({x['path'] for x in w.search('needle',glob=['*.swift'])['result']['results']},{'App.swift'})
        self.write('.gitignore','*.py\n')
        w.call('workspace/ignore_changed',{'root_id':'root'})
        self.assertEqual(w.search('needle',languages=['python'])['result']['matched_units'],0)
        self.write('.gitignore','ignored/\n')
        w.call('workspace/ignore_changed',{'root_id':'root'})
        self.assertIn('new.py',{x['path'] for x in w.search('needle')['result']['results']})
    def test_nested_ignore_precedence_and_rule_symlink_rejection(self):
        self.write('.ignore','*.py\n')
        self.write('nested/.gitignore','!app.py\n')
        self.write('nested/app.py','needle')
        w=self.worker();w.index()
        self.assertEqual(w.search('needle')['result']['matched_units'],0)
        self.write('.ignore','')
        w.call('workspace/ignore_changed',{'root_id':'root'})
        self.assertEqual(w.search('needle')['result']['matched_units'],1)
        with tempfile.TemporaryDirectory() as outside:
            rule=Path(outside)/'rules';rule.write_text('*.py\n')
            (self.root/'.ignore').unlink();(self.root/'.ignore').symlink_to(rule)
            result=w.call('workspace/ignore_changed',{'root_id':'root'})
            self.assertTrue(result['result']['incomplete'])
            self.assertEqual(result['result']['skipped_files']['counts']['read_error'],1)
            self.assertEqual(w.search('needle')['result']['matched_units'],0)

    def test_text_ranges_crlf_unicode_and_match_line_truncation(self):
        self.write('unicode.swift','func café() {\r\n'+('    print("needle")\r\n'*300)+'}\r\n')
        w=self.worker();w.index()
        hit=w.search('needle','symbol',budget_bytes=20000)['result']['results'][0]
        self.assertEqual(hit['symbol'],'café')
        self.assertEqual(hit['start_line'],1);self.assertEqual(hit['end_line'],302)
        self.assertEqual(hit['source_range']['end']['byte_column'],1)
        self.assertNotIn('\r',hit['content'])
        self.assertEqual(len(hit['match_lines']),256);self.assertTrue(hit['match_lines_truncated'])

    def test_negotiation_rejects_unrestricted_or_unknown_versions(self):
        frames=[{'id':1,'method':'initialize','params':{'protocol_version':99,'workspace_id':'nain','roots':[{'id':'root','path':str(self.root)}]}},
                {'id':2,'method':'initialize','params':{'protocol_version':1,'restricted':False,'workspace_id':'nain','roots':[{'id':'root','path':str(self.root)}]}}]
        result=subprocess.run([str(BINARY),'serve','--stdio'],input=''.join(json.dumps(f)+'\n' for f in frames),capture_output=True,text=True,check=True)
        replies=[json.loads(s) for s in result.stdout.splitlines()]
        self.assertEqual(replies[0]['error']['code'],'unsupported_protocol')
        self.assertEqual(replies[1]['error']['code'],'invalid_params')
        self.assertEqual(result.stderr,'')

    def test_targeted_changes_reuse_and_hash_rescan(self):
        self.write('a.py','first_token');self.write('b.py','unchanged_token')
        w = self.worker(); w.index()
        before = w.search('unchanged_token')['result']['results'][0]['content_hash']
        self.write('a.py','new_token')
        result = w.call('workspace/files_changed',{'root_id':'root','paths':['a.py']})['result']
        self.assertEqual(result['files_read'],1)
        self.assertEqual(w.search('first_token')['result']['matched_units'],0)
        self.assertEqual(w.search('unchanged_token')['result']['results'][0]['content_hash'],before)
        self.assertEqual(w.index()['files_read'],0)
        # Equal-length writes with preserved timestamps need explicit changed notification or rescan.
        stat=(self.root/'a.py').stat(); self.write('a.py','zzz_token');os.utime(self.root/'a.py',ns=(stat.st_atime_ns,stat.st_mtime_ns))
        self.assertEqual(w.index()['files_read'],0)
        self.assertGreaterEqual(w.index('index/rescan')['files_read'],2)
        self.assertEqual(w.search('zzz_token')['result']['matched_units'],1)
    def test_confined_roots_and_symlinks(self):
        with tempfile.TemporaryDirectory() as outside:
            Path(outside,'secret.py').write_text('outside_token')
            (self.root/'link').symlink_to(outside,target_is_directory=True)
            self.write('local.py','local_token')
            w = self.worker(other_roots=[{'id':'other','path':outside}]); w.index()
            self.assertEqual(w.search('outside_token')['result']['matched_units'],0)
            w.call('index/refresh',{'root_id':'other'})
            other=w.search('outside_token',root_id='other')['result']
            self.assertEqual(other['results'][0]['root_id'],'other')
            self.assertEqual(other['results'][0]['path'],'secret.py')
            self.assertEqual(w.search('outside_token')['result']['matched_units'],0)
            self.assertEqual(w.search('anything',root_id='missing')['error']['code'],'unknown_root')
            for path in ('../secret.py',str(Path(outside,'secret.py')),'link/secret.py','a//b.py','a/./b.py'):
                r=w.call('document/update',{'root_id':'root','path':path,'version':1,'content':'outside_token'})
                self.assertIn('error',r)
                self.assertIn('error',w.call('workspace/files_changed',{'root_id':'root','paths':[path]}))
            self.assertGreater(w.search('local_token')['result']['skipped_files']['counts']['symlink'],0)
    def test_incremental_bm25_matches_fresh_index_after_updates_and_deletions(self):
        self.write('a.py','def rareToken():\n    return "rare token"\n')
        self.write('b.py','def other():\n    return "other"\n')
        w=self.worker();w.index()
        self.write('b.py','def other():\n    return "rare token rare"\n')
        self.write('c.py','def extra():\n    return "extra token"\n')
        w.call('workspace/files_changed',{'root_id':'root','paths':['b.py','c.py']})
        fresh=self.worker();fresh.index()
        def ranking(worker):
            return [(h['path'],h['start_line'],h['score']) for h in worker.search('rare token','ranked')['result']['results']]
        self.assertEqual(ranking(w),ranking(fresh))
        (self.root/'b.py').unlink()
        w.call('workspace/files_changed',{'root_id':'root','paths':['b.py']})
        newer=self.worker();newer.index()
        self.assertEqual(ranking(w),ranking(newer))

    def test_duplicate_active_id_is_rejected_without_losing_original(self):
        for n in range(120): self.write(f'{n}.txt','needle line\n'*1000)
        w=self.worker()
        w.send('index/refresh',{'root_id':'root'},'same')
        started=w.messages.get(timeout=20)
        self.assertEqual(started['params']['phase'],'started')
        w.send('capabilities',{},'same')
        self.assertEqual(w.wait('same')['error']['code'],'duplicate_id')
        self.assertIn('result',w.wait('same'))
        self.assertTrue(w.call('capabilities',{})['result']['restricted'])

    def test_resource_limits_and_utf8_response_clipping(self):
        self.write('a.txt','needle '+'é"\\'*200)
        self.write('big.txt','needle '+'x'*2048)
        self.write('binary.txt','needle\0')
        (self.root/'bad.txt').write_bytes(b'\xffneedle')
        w = self.worker(limits={'max_file_bytes':1024,'response_bytes':4096,'memory_bytes':1024*1024})
        status=w.index();self.assertTrue(status['incomplete'])
        self.assertEqual(status['skipped_files']['counts']['file_size'],1)
        self.assertEqual(status['skipped_files']['counts']['binary'],1)
        self.assertEqual(status['skipped_files']['counts']['non_utf8'],1)
        r=w.search('needle',budget_bytes=1000)['result']
        self.assertLessEqual(len(json.dumps(r,ensure_ascii=False).encode())+1024,4096)
        r=w.search('needle',budget_bytes=9)['result']
        self.assertTrue(r['truncated']);self.assertTrue(r['results'][0]['excerpt_truncated'])
        self.assertLessEqual(sum(len(x['content'].encode()) for x in r['results']),9)
        self.assertLessEqual(status['memory_accounted_bytes'],1024*1024)
    def test_file_and_memory_limits_are_reported(self):
        for n in range(30): self.write(f'{n:03}.py','def needle():\n    return "needle"\n'*20)
        w=self.worker(limits={'max_files':2})
        status=w.index();self.assertTrue(status['incomplete']);self.assertEqual(status['files_indexed'],2)
        self.assertIn('file_limit',status['skipped_files']['counts'])
        limited=self.worker(limits={'memory_bytes':1024*1024,'max_file_bytes':1024})
        status=limited.index();self.assertLessEqual(status['memory_accounted_bytes'],1024*1024)
        self.assertIn('memory_limit',status['skipped_files']['counts'])
    def test_restricted_worker_and_cli_do_not_call_models(self):
        calls=[]
        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                calls.append(self.path);self.send_response(500);self.end_headers()
            def log_message(self,*_args): pass
        server=ThreadingHTTPServer(('127.0.0.1',11434),Handler)
        thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
        self.addCleanup(server.server_close);self.addCleanup(server.shutdown)
        trap=self.root/'.trap';trap.mkdir()
        executable=trap/'ollama';executable.write_text('#!/bin/sh\ntouch "$AGX_MODEL_TRAP"\nexit 1\n');executable.chmod(0o755)
        env={**os.environ,'PATH':str(trap),'AGX_MODEL_TRAP':str(self.root/'.model-called')}
        self.write('app.py','def needle():\n    return "needle"\n')
        w=self.worker(env=env);w.index()
        for mode in ('text','symbol','ranked'):
            self.assertGreater(w.search('needle',mode)['result']['matched_units'],0)
            cli=subprocess.run([str(BINARY),'search','needle',str(self.root),'--mode',mode],env=env,capture_output=True,check=True)
            self.assertGreater(json.loads(cli.stdout)['matched_units'],0)
            forbidden=subprocess.run([str(BINARY),'search','needle',str(self.root),'--mode',mode,'--model','fixture'],env=env,capture_output=True)
            self.assertEqual(forbidden.returncode,2)
        for mode in ('hybrid','semantic','embedding'):
            self.assertEqual(w.search('needle',mode)['error']['code'],'unsupported_mode')
        for field in ('model','embedding_model','ollama_url','inference_url','hidden'):
            self.assertIn('error',w.search('needle',**{field:'fixture'}))
        for method in ('embed','models/pull','hybrid','parse','doctor','mcp','update'):
            self.assertEqual(w.call(method,{})['error']['code'],'method_not_found')
        self.assertEqual(calls,[]);self.assertFalse((self.root/'.model-called').exists())
    def test_cancel_index_and_search_with_recovery(self):
        for n in range(120): self.write(f'{n}.txt','needle line\n'*1000)
        w=self.worker()
        request=w.send('index/refresh',{'root_id':'root'},'cancel-index')
        notification=w.messages.get(timeout=20)
        self.assertEqual(notification['params']['phase'],'started')
        w.process.stdin.write(json.dumps({'method':'cancel','params':{'request_id':request}})+'\n');w.process.stdin.flush()
        self.assertEqual(w.wait(request)['error']['code'],'cancelled')
        self.assertEqual(w.search('needle')['error']['code'],'index_not_ready')
        w.index()
        w.process.stdin.write(json.dumps({'id':'cancel-search','method':'search','params':{'root_id':'root','query':'needle','limit':1000}})+'\n'+json.dumps({'method':'cancel','params':{'request_id':'cancel-search'}})+'\n');w.process.stdin.flush()
        self.assertEqual(w.wait('cancel-search')['error']['code'],'cancelled')
        self.assertGreater(w.search('needle')['result']['matched_units'],0)
    def test_bad_frames_and_duplicate_active_ids_do_not_kill_worker(self):
        w=self.worker()
        w.process.stdin.write('not-json\n');w.process.stdin.flush()
        self.assertEqual(w.messages.get(timeout=20)['error']['code'],'parse_error')
        w.process.stdin.write('x'*(4*1024*1024+1)+'\n');w.process.stdin.flush()
        self.assertEqual(w.messages.get(timeout=20)['error']['code'],'frame_too_large')
        self.assertTrue(w.call('capabilities',{})['result']['restricted'])
        self.assertIn('error',w.call('capabilities',{'model':'fixture'}))
        self.assertIn('error',w.search('['))
        self.assertIn('error',w.search('needle',limit=0))

if __name__ == '__main__': unittest.main()
