-- A small shop for trying Atlas and for its screenshots. Works in SQLite,
-- MariaDB and PostgreSQL (plain types, explicit keys).
CREATE TABLE categories (
  id INTEGER PRIMARY KEY,
  parent_id INTEGER REFERENCES categories(id),
  name VARCHAR(80) NOT NULL,
  slug VARCHAR(80) NOT NULL UNIQUE
);
CREATE TABLE suppliers (
  id INTEGER PRIMARY KEY,
  name VARCHAR(120) NOT NULL,
  country CHAR(2) NOT NULL,
  email VARCHAR(160)
);
CREATE TABLE products (
  id INTEGER PRIMARY KEY,
  category_id INTEGER NOT NULL REFERENCES categories(id),
  sku VARCHAR(32) NOT NULL UNIQUE,
  title VARCHAR(160) NOT NULL,
  description TEXT,
  price_cents INTEGER NOT NULL,
  weight_grams INTEGER,
  attributes TEXT,
  active BOOLEAN NOT NULL DEFAULT TRUE,
  created_at TIMESTAMP NOT NULL,
  updated_at TIMESTAMP
);
CREATE TABLE product_suppliers (
  product_id INTEGER NOT NULL REFERENCES products(id),
  supplier_id INTEGER NOT NULL REFERENCES suppliers(id),
  cost_cents INTEGER NOT NULL,
  PRIMARY KEY (product_id, supplier_id)
);
CREATE TABLE customers (
  id INTEGER PRIMARY KEY,
  email VARCHAR(160) NOT NULL UNIQUE,
  first_name VARCHAR(80) NOT NULL,
  last_name VARCHAR(80) NOT NULL,
  phone VARCHAR(40),
  created_at TIMESTAMP NOT NULL
);
CREATE TABLE addresses (
  id INTEGER PRIMARY KEY,
  customer_id INTEGER NOT NULL REFERENCES customers(id),
  street VARCHAR(160) NOT NULL,
  postcode VARCHAR(12) NOT NULL,
  city VARCHAR(80) NOT NULL,
  country CHAR(2) NOT NULL
);
CREATE TABLE coupons (
  id INTEGER PRIMARY KEY,
  code VARCHAR(32) NOT NULL UNIQUE,
  percent INTEGER NOT NULL,
  valid_until DATE
);
CREATE TABLE orders (
  id INTEGER PRIMARY KEY,
  customer_id INTEGER NOT NULL REFERENCES customers(id),
  shipping_address_id INTEGER REFERENCES addresses(id),
  coupon_id INTEGER REFERENCES coupons(id),
  status VARCHAR(16) NOT NULL,
  placed_at TIMESTAMP NOT NULL,
  total_cents INTEGER NOT NULL
);
CREATE TABLE order_items (
  order_id INTEGER NOT NULL REFERENCES orders(id),
  product_id INTEGER NOT NULL REFERENCES products(id),
  quantity INTEGER NOT NULL,
  price_cents INTEGER NOT NULL,
  PRIMARY KEY (order_id, product_id)
);
CREATE TABLE payments (
  id INTEGER PRIMARY KEY,
  order_id INTEGER NOT NULL UNIQUE REFERENCES orders(id),
  method VARCHAR(16) NOT NULL,
  amount_cents INTEGER NOT NULL,
  paid_at TIMESTAMP
);
CREATE TABLE shipments (
  id INTEGER PRIMARY KEY,
  order_id INTEGER NOT NULL REFERENCES orders(id),
  carrier VARCHAR(40) NOT NULL,
  tracking VARCHAR(64),
  shipped_at TIMESTAMP
);
CREATE TABLE reviews (
  id INTEGER PRIMARY KEY,
  product_id INTEGER NOT NULL REFERENCES products(id),
  customer_id INTEGER NOT NULL REFERENCES customers(id),
  stars INTEGER NOT NULL,
  body TEXT,
  created_at TIMESTAMP NOT NULL
);
-- No declared key: Atlas infers it from the name.
CREATE TABLE wishlist (
  id INTEGER PRIMARY KEY,
  customer_id INTEGER NOT NULL,
  product_id INTEGER NOT NULL,
  added_at TIMESTAMP NOT NULL
);
