import sqlite3
c=sqlite3.connect(':memory:')
c.execute('create table t(v int)')
c.executemany('insert into t values(?)',[(i,) for i in range(1,1001)])
print('sum', c.execute('select sum(v) from t').fetchone()[0])
