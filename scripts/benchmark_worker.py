#!/usr/bin/env python3
"""Reproducible CLI/worker timings; synthetic corpus, no agent-quality claims."""
import argparse
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import tempfile
import time

class Client:
    def __init__(self, binary):
        self.process=subprocess.Popen([binary,'serve','--stdio','--restricted'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        self.number=0
    def call(self, method, params):
        self.number+=1
        started=time.perf_counter()
        self.process.stdin.write(json.dumps({'id':self.number,'method':method,'params':params})+'\n');self.process.stdin.flush()
        while True:
            line=self.process.stdout.readline()
            if not line: raise RuntimeError('worker closed: '+self.process.stderr.read())
            value=json.loads(line)
            if value.get('id')==self.number:
                if 'error' in value: raise RuntimeError(value['error'])
                return value['result'],(time.perf_counter()-started)*1000,len(line.encode())
    def close(self):
        self.process.stdin.close();self.process.wait(timeout=30)
        errors=self.process.stderr.read();self.process.stdout.close();self.process.stderr.close()
        if self.process.returncode: raise RuntimeError(errors)

def distribution(samples, size, runs):
    return {'median_ms':round(statistics.median(samples),3),'min_ms':round(min(samples),3),'max_ms':round(max(samples),3),'stdout_bytes':size,'runs':runs}

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--binary',default='target/release/agx')
    parser.add_argument('--files',type=int,default=1000)
    parser.add_argument('--runs',type=int,default=5)
    parser.add_argument('--out',type=Path)
    args=parser.parse_args()
    if not 1<=args.files<=10000 or not 1<=args.runs<=100: parser.error('files 1..10000, runs 1..100')
    binary=str(Path(args.binary).resolve(strict=True))
    with tempfile.TemporaryDirectory(prefix='agx-editor-bench-') as directory:
        root=Path(directory)
        for n in range(args.files):
            path=root/'src'/f'module_{n//50}'/f'file_{n}.py';path.parent.mkdir(parents=True,exist_ok=True)
            term='session_token' if n%20==0 else 'render_canvas'
            path.write_text(''.join(f'def {term}_{n}_{j}(value):\n    # {term} implementation\n    return value\n' for j in range(20)))
        report={'platform':{'system':platform.system(),'machine':platform.machine(),'python':platform.python_version(),'agx':subprocess.check_output([binary,'--version'],text=True).strip()},'corpus':{'files':args.files,'lines_per_file':60,'language':'python','total_bytes':sum(p.stat().st_size for p in root.rglob('*.py'))},'measurements':{}}
        measurements=report['measurements']
        started=time.perf_counter();client=Client(binary)
        try:
            info,_,_=client.call('initialize',{'protocol_version':1,'workspace_id':'benchmark','roots':[{'id':'root','path':directory}],'limits':{'memory_bytes':256*1024*1024}})
            measurements['worker_start_initialize']={'ms':round((time.perf_counter()-started)*1000,3)}
            report['worker_limits']=info['limits']
            status,elapsed,size=client.call('index/refresh',{'root_id':'root'})
            measurements['worker_index_cold']={'ms':round(elapsed,3),'files_read':status['files_read'],'files_reused':status['files_reused'],'files_indexed':status['files_indexed'],'incomplete':status['incomplete'],'memory_accounted_bytes':status['memory_accounted_bytes']}
            for mode in ('text','symbol','ranked'):
                query='session token' if mode=='ranked' else 'session_token'
                samples=[]
                for _ in range(args.runs):
                    result,elapsed,size=client.call('search',{'root_id':'root','query':query,'mode':mode,'limit':8})
                    samples.append(elapsed)
                measurements[f'worker_{mode}_warm']=distribution(samples,size,args.runs)
                measurements[f'worker_{mode}_warm'].update({'matched_units':result['matched_units'],'returned_units':result['returned_units'],'incomplete':result['incomplete']})
                cli=[binary,'search',query,directory,'--mode',mode,'--limit','8']
                samples=[]
                for run in range(args.runs+1):
                    started=time.perf_counter();cli_result=subprocess.run(cli,check=True,capture_output=True);elapsed=(time.perf_counter()-started)*1000
                    if run==0: measurements[f'cli_{mode}_first']={'ms':round(elapsed,3),'stdout_bytes':len(cli_result.stdout)}
                    else: samples.append(elapsed)
                measurements[f'cli_{mode}_warm']=distribution(samples,len(cli_result.stdout),args.runs)
            status,elapsed,_=client.call('index/refresh',{'root_id':'root'})
            measurements['worker_metadata_refresh']={'ms':round(elapsed,3),'files_read':status['files_read'],'files_reused':status['files_reused']}
            changed=root/'src/module_0/file_0.py';changed.write_text(changed.read_text()+'\nchanged_token = 1\n')
            status,elapsed,_=client.call('workspace/files_changed',{'root_id':'root','paths':['src/module_0/file_0.py']})
            measurements['worker_one_file_update']={'ms':round(elapsed,3),'files_read':status['files_read'],'files_reused':status['files_reused']}
            result,_,_=client.call('search',{'root_id':'root','query':'changed_token'})
            assert result['matched_units']>0, 'changed-file correctness failed'
            try:
                rss=subprocess.check_output(['ps','-o','rss=','-p',str(client.process.pid)],text=True).strip()
                report['worker_observed_rss_kib']=int(rss)
            except (OSError,ValueError,subprocess.CalledProcessError): pass
        finally: client.close()
        subprocess.run([binary,'clean',directory],check=True,capture_output=True)
        report['limitations']=['Synthetic Python corpus; not a task-success, semantic-recall, or real-project study.','First/cold means a new process/index; filesystem pages may already be cached by corpus creation.','CLI timings include startup; warm worker queries exclude startup and initial indexing.','CLI schema 1 and worker schema 2 have different metadata and BM25 tie/scope behavior; output byte counts are not equivalent workloads.','Warm worker queries scan cached documents/chunks; there is no inverted-postings candidate index.','The worker does no query-time disk freshness check: the editor must report events, or request refresh/rescan.','Cache accounting is conservative, not an RSS cap; transient parsing, protocol queues, and allocator/runtime overhead are additional.','No inference services or embedding models were used.']
        text=json.dumps(report,indent=2)+'\n'
        if args.out: args.out.parent.mkdir(parents=True,exist_ok=True);args.out.write_text(text)
        print(text,end='')

if __name__=='__main__':main()
