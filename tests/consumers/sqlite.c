
typedef struct sqlite3 sqlite3;
extern int sqlite3_open_v2(const char*,sqlite3**,int,const char*);
extern int sqlite3_exec(sqlite3*,const char*,int(*)(void*,int,char**,char**),void*,char**);
extern int sqlite3_close(sqlite3*);
extern const char *sqlite3_libversion(void);
extern int pipe(int*),fork(void),waitpid(int,int*,int),close(int);
extern long read(int,void*,unsigned long),write(int,const void*,unsigned long);
extern void _exit(int);
#define CHECK(x,n) do{if(!(x)){write(2,"sqlite check failed\n",20);return n;}}while(0)
static int open_db(sqlite3 **db){return sqlite3_open_v2("/work/cache.db",db,2|4,0);}
static int value;
static int integrity_ok;
static int integrity(void *p,int n,char **v,char **names){(void)p;(void)names;integrity_ok=n==1 && v[0] && v[0][0]=='o' && v[0][1]=='k' && v[0][2]==0;return !integrity_ok;}
static int collect(void *p,int n,char **v,char **names){(void)p;(void)names;if(n!=1 || !v[0])return 1;value=0;for(char*c=v[0];*c;c++)value=value*10+*c-'0';return 0;}
int main(void){
 sqlite3 *db=0;CHECK(sqlite3_libversion()!=0,10);CHECK(open_db(&db)==0,11);
 CHECK(sqlite3_exec(db,"PRAGMA journal_mode=DELETE; CREATE TABLE items(v INTEGER);",0,0,0)==0,12);
 CHECK(sqlite3_close(db)==0,13);
 int go[2],ready[2];CHECK(pipe(go)==0 && pipe(ready)==0,14);int child=fork();CHECK(child>=0,15);
 if(child==0){
  close(go[1]);close(ready[0]);char marker=0;CHECK(read(go[0],&marker,1)==1,16);
  CHECK(open_db(&db)==0,17);CHECK(sqlite3_exec(db,"BEGIN IMMEDIATE;",0,0,0)==5,18);
  CHECK(write(ready[1],"B",1)==1 && read(go[0],&marker,1)==1,19);
  CHECK(sqlite3_exec(db,"BEGIN IMMEDIATE; INSERT INTO items VALUES(22); COMMIT;",0,0,0)==0,20);
  CHECK(sqlite3_close(db)==0,21);_exit(0);
 }
 close(go[0]);close(ready[1]);CHECK(open_db(&db)==0,22);
 CHECK(sqlite3_exec(db,"BEGIN IMMEDIATE; INSERT INTO items VALUES(11);",0,0,0)==0,23);
 CHECK(write(go[1],"G",1)==1,24);char marker=0;CHECK(read(ready[0],&marker,1)==1 && marker=='B',25);
 CHECK(sqlite3_exec(db,"COMMIT;",0,0,0)==0 && sqlite3_close(db)==0,26);
 CHECK(write(go[1],"G",1)==1,27);int status=-1;CHECK(waitpid(child,&status,0)==child && status==0,28);
 CHECK(open_db(&db)==0 && sqlite3_exec(db,"SELECT sum(v) FROM items;",collect,0,0)==0 && value==33,29);
 CHECK(sqlite3_exec(db,"PRAGMA integrity_check;",integrity,0,0)==0 && integrity_ok && sqlite3_close(db)==0,30);
 CHECK(close(go[1])==0 && close(ready[0])==0,31);write(1,"sqlite-locks: PASS\n",19);return 0;
}
