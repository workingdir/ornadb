#!/usr/bin/env python3
"""Focused reproduction of EBNF/prose discrepancies; NOT an Orna compiler.
Converts the shipped syntactic EBNF to Lark. Literal-token recognisers are
replaced with bounded ASCII recognisers sufficient for the listed probes.
No claim is made about Unicode, interpolation, typechecking or recovery.
"""
from pathlib import Path
import ast, json, re, sys
from lark import Lark
ROOT=Path(sys.argv[1]) if len(sys.argv)>1 else Path(__file__).resolve().parents[1]
src=(ROOT/'grammar/orna.ebnf').read_text()
src=re.sub(r'\(\*[\s\S]*?\*\)', '', src)
pattern=re.compile(r'\s+|"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])*\'|\?[^?]*\?|[A-Za-z_][A-Za-z0-9_]*|[=;,|\[\]{}()]')
tokens=[]; pos=0
for m in pattern.finditer(src):
 if src[pos:m.start()].strip():raise ValueError(src[pos:m.start()])
 t=m.group();pos=m.end()
 if not t.isspace():tokens.append(t)
if src[pos:].strip():raise ValueError(src[pos:])
i=0; definitions={}
def parse_seq(stops):
 global i
 parts=[]
 while i<len(tokens) and tokens[i] not in stops:
  t=tokens[i];i+=1
  if t==',':continue
  if t in ('[','{','('):
   end={'[':']','{':'}','(':')'}[t]
   inner=parse_alt({end});assert tokens[i]==end;i+=1
   parts.append('('+inner+')'+({'[':'?','{':'*','(' :''}[t]))
  elif t.startswith(('"',"'")):parts.append(json.dumps(ast.literal_eval(t)))
  elif t.startswith('?'):parts.append('SPECIAL')
  else:parts.append(t)
 return ' '.join(parts)
def parse_alt(stops):
 global i
 parts=[parse_seq(stops|{'|'})]
 while i<len(tokens) and tokens[i]=='|':
  i+=1;parts.append(parse_seq(stops|{'|'}))
 return ' | '.join(parts)
while i<len(tokens):
 name=tokens[i];i+=1;assert tokens[i]=='=';i+=1
 val=parse_alt({';'});assert tokens[i]==';';i+=1;definitions[name]=val
lexical_start=list(definitions).index('identifier')
syntax={k:v for k,v in list(definitions.items())[:lexical_start]}
words=sorted(set(re.findall(r'"([a-z_]+)"',definitions['contextual_name'])), key=lambda x:(-len(x),x))
# Exclude exactly the declared keyword set in the bounded ASCII probe.
reserved='|'.join(re.escape(x) for x in words)
extra=r'''
identifier: IDENTIFIER
integer_literal: INT
float_literal: FLOAT
decimal_source_numeral: DECIMAL
signed_integer: ("+"|"-")? INT
date_literal: DATE
instant_literal: INSTANT
string_literal: STRING
empty:
INT: /(?:0x[0-9a-fA-F_]+|0b[01_]+|[0-9][0-9_]*)/
FLOAT: /[0-9][0-9_]*(?:\.[0-9][0-9_]*)?(?:[eE][+-]?[0-9][0-9_]*)?f/
DECIMAL: /(?:[0-9][0-9_]*\.[0-9][0-9_]*(?:[eE][+-]?[0-9][0-9_]*)?|[0-9][0-9_]*[eE][+-]?[0-9][0-9_]*)/
DATE: /[0-9]{4}-[0-9]{2}-[0-9]{2}/
INSTANT: /[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?(?:Z|[+-][0-9]{2}:[0-9]{2})/
STRING: /"(?:\\.|[^"\\])*"/
%ignore /[ \t\r\n]+/
'''
grammar='\n'.join(f'{k}: {v}' for k,v in syntax.items())+'\n'+extra+'\nIDENTIFIER: /(?!(?:'+reserved+r')\b)[A-Za-z_][A-Za-z0-9_]*/'+'\n'

parser=Lark(grammar,parser='earley',lexer='dynamic',ambiguity='resolve',start=['module_unit','row_unit','repl_input'])
comment_pattern=re.compile(r'"(?:\\.|[^"\\])*"|//[^\n]*|/\*[\s\S]*?\*/')
def parse(source,entry='module_unit'):
    source=comment_pattern.sub(lambda m:m.group() if m.group().startswith('"') else '\n'*m.group().count('\n')+' ',source)
    return parser.parse(source,start=entry)
def main():
    manifest=json.loads((ROOT/'tests/conformance-manifest.json').read_text()); results=[]
    for fixture in manifest['fixtures']:
        if fixture['kind'] not in ('valid','invalid'):continue
        path=ROOT/fixture['path']
        if not path.exists():continue
        expect=fixture['expect']['parse']=='pass'
        try: parse(path.read_text(),fixture['parse_as']); accepted=True; error=None
        except Exception as ex: accepted=False; error=str(ex)[:900]
        results.append({'path':fixture['path'],'entrypoint':fixture['parse_as'],'expected':expect,'accepted':accepted,'agrees':expect==accepted,'error':error})
    report={'scope':'Bounded EBNF syntax probe, not an Orna compiler. ASCII lexical recognisers; strings opaque; no interpolation, Unicode-profile, type/effect checking or evaluator.', 'count':len(results),'agree':sum(r['agrees'] for r in results),'results':results}
    (ROOT/'evidence/syntax-probe.json').write_text(json.dumps(report,indent=2)+'\n')
    (ROOT/'evidence/probe-grammar.lark').write_text(grammar)
    print({k:v for k,v in report.items() if k!='results'})
    for r in results:
        if not r['agrees']:print(r['path'], 'expected',r['expected'],r['error'])
    return 0 if all(r['agrees'] for r in results) else 1
if __name__=='__main__':raise SystemExit(main())
