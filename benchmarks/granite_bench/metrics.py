import math,re,unicodedata
from collections import Counter

def norm(s):
    s=unicodedata.normalize('NFKC',str(s)).casefold()
    return ' '.join(re.findall(r'\w+',s))
def edit(a,b):
    old=list(range(len(b)+1))
    for i,x in enumerate(a,1):
        new=[i]+[0]*len(b)
        for j,y in enumerate(b,1):new[j]=min(new[j-1]+1,old[j]+1,old[j-1]+(x!=y))
        old=new
    return old[-1]
def wer(pred,ref):
    words=norm(ref).split();return edit(norm(pred).split(),words)/max(1,len(words))
def f1(pred,ref):
    p=Counter(norm(pred).split());r=Counter(norm(ref).split());same=sum((p&r).values())
    if not p and not r:return 1.0
    return 2*same/max(1,sum(p.values())+sum(r.values()))
def ndcg(ranked,relevance,k=10):
    def dcg(s):return sum((2**float(rel)-1)/math.log2(i+2) for i,rel in enumerate(s))
    actual=[relevance.get(x,0) for x in ranked[:k]];ideal=sorted(relevance.values(),reverse=True)[:k]
    return dcg(actual)/dcg(ideal) if dcg(ideal) else 0.0
def recall(ranked,relevance,k=10):
    relevant={key for key,val in relevance.items() if val>0}
    return len(relevant.intersection(ranked[:k]))/len(relevant) if relevant else 0.0
def mase(pred,ref,history,seasonality=1):
    scale=sum(abs(history[i]-history[i-seasonality]) for i in range(seasonality,len(history)))/max(1,len(history)-seasonality)
    return sum(abs(a-b) for a,b in zip(pred,ref))/max(1,len(ref))/max(scale,1e-12)
def wql(quantiles,ref):
    errs=[]
    for q,pred in quantiles.items():
        q=float(q)
        errs.extend([(q*(y-p) if y>=p else (1-q)*(p-y)) for y,p in zip(ref,pred)])
    return 2*sum(errs)/max(1,len(quantiles)*sum(abs(y) for y in ref))
def calibration(quantiles,ref):
    return {str(q):sum(y<=p for y,p in zip(ref,preds))/max(1,len(ref)) for q,preds in quantiles.items()}
def classification(pred,truth):
    pairs=list(zip(pred,truth));tp=sum(p==1 and t==1 for p,t in pairs);fp=sum(p==1 and t==0 for p,t in pairs);fn=sum(p==0 and t==1 for p,t in pairs);tn=sum(p==0 and t==0 for p,t in pairs)
    return {'precision':tp/max(1,tp+fp),'recall':tp/max(1,tp+fn),'fpr':fp/max(1,fp+tn),'tp':tp,'fp':fp,'fn':fn,'tn':tn}
