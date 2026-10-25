import json,threading,unittest,tempfile,os
from http.server import HTTPServer,BaseHTTPRequestHandler
from granite_bench.runner import request,evaluate
class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        length=int(self.headers.get('Content-Length','0'))
        payload=json.loads(self.rfile.read(length))
        if self.path=='/v1/chat/completions':body={'choices':[{'message':{'content':'4'}}],'usage':{'completion_tokens':1}}
        elif self.path=='/v1/forecast':body={'forecast':[6,7],'quantiles':{'0.1':[5,6],'0.5':[6,7],'0.9':[7,8]}}
        else:body={'flagged':False}
        data=json.dumps(body).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    def log_message(self,*args):pass
class Protocol(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server=HTTPServer(('127.0.0.1',0),Handler);cls.thread=threading.Thread(target=cls.server.serve_forever,daemon=True);cls.thread.start()
    @classmethod
    def tearDownClass(cls):cls.server.shutdown();cls.server.server_close()
    def test_chat(self):
        response,latency,ttft=request(f'http://127.0.0.1:{self.server.server_port}',{'path':'/v1/chat/completions','request':{'model':'mock'}},5,None)
        self.assertEqual(response['text'],'4');self.assertGreaterEqual(latency,0);self.assertIsNone(ttft)
    def test_forecast(self):
        response,_,_=request(f'http://127.0.0.1:{self.server.server_port}',{'path':'/v1/forecast','request':{'model':'mock'}},5,None)
        self.assertEqual(evaluate('timeseries',{'history':[1,2,3,4,5],'future':[6,7]},response)['mase'],0)
if __name__=='__main__':unittest.main()
