import sqlite3
c=sqlite3.connect(':memory:')
c.execute('create table t(n int)')
c.executemany('insert into t values(?)',[(i,) for i in range(1,901)])
print('distinct', c.execute('select count(distinct n%30) from t').fetchone()[0])
