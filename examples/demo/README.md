# Demo database

An invented online store for trying LumeDB and taking screenshots: 67 tables with 96 foreign keys
(customers, catalog, inventory, orders, payments, shipping, HR, support, marketing), sample data,
5 views, 2 functions, 2 procedures, 3 triggers and 2 events. The data is deterministic.

```sh
docker run -d --name lumedb-demo-mysql -e MYSQL_ROOT_PASSWORD=lumedb \
  -p 127.0.0.1:33310:3306 -v lumedb-demo-mysql:/var/lib/mysql mysql:latest
python3 examples/demo/generate.py > examples/demo/lumedb_demo.mysql.sql
docker exec -i lumedb-demo-mysql mysql -uroot -plumedb < examples/demo/lumedb_demo.mysql.sql
```

Then add a MySQL connection to `127.0.0.1:33310`, user `root`, password `lumedb`, database
`lumedb_demo`.
