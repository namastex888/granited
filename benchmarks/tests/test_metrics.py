import unittest
from granite_bench import metrics as m
class Metrics(unittest.TestCase):
    def test_wer(self):self.assertAlmostEqual(m.wer('hello world','hello brave world'),1/3)
    def test_f1(self):self.assertEqual(m.f1('a b','a b'),1)
    def test_recall(self):self.assertEqual(m.recall(['a'],{'a':2,'b':1}),.5)
    def test_ndcg(self):self.assertAlmostEqual(m.ndcg(['a','b'],{'a':2,'b':1}),1)
    def test_mase(self):self.assertEqual(m.mase([4],[4],[1,2,3]),0)
    def test_guardian(self):self.assertEqual(m.classification([1,0],[0,0])['fpr'],.5)
if __name__=='__main__':unittest.main()
