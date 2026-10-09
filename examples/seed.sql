-- Demo schema for trying qbench:
--   docker run -d --name qbench-demo -e POSTGRES_PASSWORD=qbench -p 55432:5432 docker.io/library/postgres:17-alpine
--   psql postgres://postgres:qbench@localhost:55432/postgres -f examples/seed.sql
--   qbench postgres://postgres:qbench@localhost:55432/postgres

drop schema if exists shop cascade;
drop table if exists public.users, public.notes cascade;
drop type if exists public.mood;

create type public.mood as enum ('happy', 'sleepy', 'grumpy', 'hungry', 'curious');

create table public.users (
    id serial primary key,
    email text not null unique,
    display_name varchar(64),
    mood mood not null default 'curious',
    is_admin boolean not null default false,
    newsletter boolean,
    score numeric(10, 2) default 0,
    created_at timestamptz not null default now()
);

insert into public.users (email, display_name, mood, is_admin, newsletter, score)
select format('user%s@example.com', g),
       (array['Ada', 'Grace', 'Linus', 'Ken', 'Barbara', 'Edsger', 'Margaret', 'Dennis'])[1 + g % 8] || ' ' || g,
       (enum_range(null::mood))[1 + g % 5],
       g % 17 = 0,
       case when g % 3 = 0 then null else g % 2 = 0 end,
       round((random() * 1000)::numeric, 2)
from generate_series(1, 450) g;

-- No primary key: rows are addressed by ctid.
create table public.notes (
    body text,
    pinned boolean default false
);
insert into public.notes values ('remember the milk', true), ('multi
line note', false), (null, null);

create schema shop;
create type shop.order_status as enum ('pending', 'paid', 'shipped', 'delivered', 'refunded');

create table shop.orders (
    id bigint generated always as identity primary key,
    user_id int references public.users (id),
    status shop.order_status not null default 'pending',
    total_cents int not null,
    gift boolean not null default false,
    note text
);
insert into shop.orders (user_id, status, total_cents, gift)
select 1 + g % 450, (enum_range(null::shop.order_status))[1 + g % 5], (g * 137) % 50000, g % 7 = 0
from generate_series(1, 1200) g;

create view shop.big_orders as select * from shop.orders where total_cents > 40000;
